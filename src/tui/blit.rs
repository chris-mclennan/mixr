//! Native/integrated mode under tmnl — mixr renders into a ratatui
//! `TestBackend` and ships each frame's cell grid as a binary `Frame`
//! over a Unix socket to a tmnl server. Symmetric to the normal
//! `CrosstermBackend` main loop but with a UDS sink instead of stdout.
//!
//! tmnl forwards keyboard / mouse / scroll / resize as `InputEvent`s on
//! the same socket; we drain those into `App::handle_key` /
//! `App::handle_mouse` — the same paths the crossterm loop uses. End
//! result: mixr runs inside tmnl's wgpu-rendered window with the
//! IDE-style fast path, not via vt100+pty.

use std::io::BufReader;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::Path;
// Windows: AF_UNIX support landed in Win10 17063, but std doesn't
// expose `UnixStream`. `uds_windows` is a thin wrapper around the
// winapi calls with the same std-shaped API.
use std::sync::Mutex;
use std::sync::mpsc::{TryRecvError, channel};
use std::thread;
use std::time::Duration;
#[cfg(windows)]
use uds_windows::UnixStream;

use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{
    KeyCode as CtKeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers,
    MouseButton as CtMouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use tokio::sync::mpsc;
use tokio::time::sleep;

use tmnl_protocol::{
    BUTTON_LEFT, BUTTON_MIDDLE, BUTTON_RIGHT, DiffRun, Frame, InputEvent, KeyCode as WireKeyCode,
    KeyInput, MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER, Message, MouseInput, MouseKind,
    PROTOCOL_VERSION, WireCell, pack_rgba_u8, read_message, write_message,
};

use crate::tui::app::{App, AppAction};

const POLL_SLEEP_MS: u64 = 16;
const INITIAL_RESIZE_TIMEOUT: Duration = Duration::from_secs(5);

const ATTR_BOLD: u32 = 1 << 0;
const ATTR_DIM: u32 = 1 << 1;
const ATTR_ITALIC: u32 = 1 << 2;
const ATTR_UNDERLINE: u32 = 1 << 3;
const ATTR_REVERSED: u32 = 1 << 4;
const ATTR_CROSSED_OUT: u32 = 1 << 5;

/// Host theme colors received from the tmnl server via
/// `Message::Palette` — each a packed-rgba u32, the same format
/// `color_to_rgba` emits. When set, `color_to_rgba` remaps mixr's
/// role colors onto these so the panel blends into its container
/// (e.g. the mnml editor body) instead of clashing with it.
#[derive(Clone, Copy)]
struct HostPalette {
    bg: u32,
    fg: u32,
    accent: u32,
}

/// Run mixr in native (blit) mode against the tmnl-server listening on
/// `socket`. Drains tmnl's Input events into the app, emits per-tick
/// Frames over the UDS.
pub async fn run(
    mut app: App,
    mut action_rx: mpsc::UnboundedReceiver<AppAction>,
    socket: &Path,
) -> Result<()> {
    let conn = UnixStream::connect(socket)
        .map_err(|e| anyhow::anyhow!("blit: connect {}: {e}", socket.display()))?;
    let reader_stream = conn
        .try_clone()
        .map_err(|e| anyhow::anyhow!("blit: clone stream: {e}"))?;
    let writer = Mutex::new(conn);

    // Hello — same handshake mnml does. `caps` was added in
    // tmnl-protocol 0.0.6 to negotiate optional features (client
    // commands etc.). mixr's blit mode is a basic frame renderer
    // for now — no extras — so we advertise an empty caps bitmask.
    {
        let mut w = writer.lock().unwrap();
        write_message(
            &mut *w,
            &Message::Hello {
                version: PROTOCOL_VERSION,
                caps: tmnl_protocol::Caps::empty(),
            },
        )
        .map_err(|e| anyhow::anyhow!("blit: hello: {e}"))?;
    }

    // Reader thread → sync mpsc (cheap to drain non-blockingly from the
    // async loop). tokio's UnixStream would force every consumer to be
    // async too — not worth it for a single bg reader.
    let (resize_tx, resize_rx) = channel::<(u16, u16)>();
    let (input_tx, input_rx) = channel::<InputEvent>();
    let (quit_tx, quit_rx) = channel::<()>();
    let (disc_tx, disc_rx) = channel::<()>();
    let (palette_tx, palette_rx) = channel::<HostPalette>();
    thread::spawn(move || {
        let mut r = BufReader::new(reader_stream);
        loop {
            match read_message(&mut r) {
                Ok(Message::Resize(rz)) => {
                    if resize_tx.send((rz.cols, rz.rows)).is_err() {
                        break;
                    }
                }
                Ok(Message::Input(ev)) => {
                    if input_tx.send(ev).is_err() {
                        break;
                    }
                }
                Ok(Message::Palette { bg, fg, accent }) => {
                    let _ = palette_tx.send(HostPalette { bg, fg, accent });
                }
                Ok(Message::Quit) => {
                    let _ = quit_tx.send(());
                    break;
                }
                Ok(_) => {}
                Err(_) => {
                    let _ = disc_tx.send(());
                    break;
                }
            }
        }
    });

    // Wait for tmnl's initial Resize before building the TestBackend so
    // the first draw lands at the right dims. 5 s is generous — handshake
    // already succeeded, the Resize follows immediately on the host side.
    let recv_resize = || -> Result<(u16, u16)> {
        let deadline = std::time::Instant::now() + INITIAL_RESIZE_TIMEOUT;
        loop {
            match resize_rx.try_recv() {
                Ok(p) => return Ok(p),
                Err(TryRecvError::Empty) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(anyhow::anyhow!("blit: no Resize from server within 5s"));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(TryRecvError::Disconnected) => {
                    return Err(anyhow::anyhow!("blit: server disconnected"));
                }
            }
        }
    };
    let (mut cols, mut rows) = recv_resize()?;
    if cols == 0 || rows == 0 {
        return Err(anyhow::anyhow!(
            "blit: server reported empty grid {cols}x{rows}"
        ));
    }

    let backend = TestBackend::new(cols, rows);
    let mut terminal =
        Terminal::new(backend).map_err(|e| anyhow::anyhow!("blit: terminal: {e}"))?;

    let mut frame_seq: u64 = 0;
    let mut prev_cells: Vec<WireCell> = Vec::new();
    let mut prev_dims: (u16, u16) = (0, 0);
    // Host theme palette — `None` until the server sends `Message::Palette`
    // (right after the connect handshake), then mixr re-themes to match.
    let mut host_palette: Option<HostPalette> = None;

    loop {
        // Drain resize (most recent wins; same shape as mnml's blit).
        let mut new_size: Option<(u16, u16)> = None;
        loop {
            match resize_rx.try_recv() {
                Ok(p) => new_size = Some(p),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
        if let Some((nc, nr)) = new_size
            && (nc != cols || nr != rows)
            && nc > 0
            && nr > 0
        {
            cols = nc;
            rows = nr;
            // TestBackend's buffer storage doesn't follow Terminal::resize
            // alone — has to be resized explicitly first or the next draw
            // indexes past the storage. (Same bug we hit in mnml.)
            terminal.backend_mut().resize(cols, rows);
            terminal
                .resize(Rect::new(0, 0, cols, rows))
                .map_err(|e| anyhow::anyhow!("blit: resize: {e}"))?;
            prev_cells.clear();
        }

        // Drain any palette update from the host (most recent wins).
        // A disconnected channel just means the reader ended — keep
        // the last palette; the loop exits below via `disc_rx`.
        while let Ok(p) = palette_rx.try_recv() {
            host_palette = Some(p);
        }

        // tmnl sent Quit, or the reader died → clean exit.
        if quit_rx.try_recv().is_ok() {
            return Ok(());
        }
        if disc_rx.try_recv().is_ok() {
            return Ok(());
        }

        // Drain pending input events.
        loop {
            match input_rx.try_recv() {
                Ok(InputEvent::Key(k)) => {
                    let ke = key_to_crossterm(&k);
                    // mixr's quit chord matches the crossterm path's:
                    // Ctrl+C breaks out of the loop.
                    if ke.code == CtKeyCode::Char('c')
                        && ke.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        return Ok(());
                    }
                    app.handle_key(ke);
                }
                Ok(InputEvent::Mouse(m)) => {
                    let me = mouse_to_crossterm(&m);
                    app.handle_mouse(me);
                }
                // Rich-input variants added in tmnl-protocol 0.0.9.
                // mixr doesn't react to focus / hover / IME yet — silently
                // drop. Wire up later if we need to dim on focus loss
                // or surface IME composition state.
                Ok(InputEvent::Focus(_)) | Ok(InputEvent::Hover(_)) | Ok(InputEvent::Ime(_)) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }

        // Drain queued AppActions — same as the crossterm loop.
        while let Ok(action) = action_rx.try_recv() {
            app.handle_action(action).await;
        }

        app.tick().await;

        // Sentinel-trick to detect "did any widget call
        // `frame.set_cursor_position` this frame". Set the terminal
        // cursor to a known out-of-band value BEFORE draw; if it's
        // still there after, no widget moved it and we want the
        // cursor hidden over the wire. Otherwise the position is
        // the actively-set one.
        //
        // Why: ratatui's `terminal.get_cursor_position` returns the
        // last-set cursor regardless of whether anyone set it this
        // frame, so dashboard renders (no input) would otherwise
        // ship a visible cursor at the stale (0, 0) default and
        // tmnl paints a white block under the top-left corner.
        const CURSOR_SENTINEL: ratatui::layout::Position = ratatui::layout::Position {
            x: u16::MAX,
            y: u16::MAX,
        };
        terminal.set_cursor_position(CURSOR_SENTINEL).ok();
        terminal
            .draw(|frame| app.render(frame))
            .map_err(|e| anyhow::anyhow!("blit: draw: {e}"))?;
        let raw_cursor = terminal.get_cursor_position().ok();
        let cursor = match raw_cursor {
            Some(p) if p == CURSOR_SENTINEL => None,
            other => other,
        };

        let buf = terminal.backend().buffer();
        let bw = buf.area.width;
        let bh = buf.area.height;
        let mut cells = Vec::with_capacity(bw as usize * bh as usize);
        for y in 0..bh {
            for x in 0..bw {
                let c = &buf[(x, y)];
                let fg = color_to_rgba(c.fg, false, host_palette);
                let bg = color_to_rgba(c.bg, true, host_palette);
                let ch = c.symbol().chars().next().unwrap_or(' ') as u32;
                let attrs = modifier_to_bits(c.modifier);
                cells.push(WireCell { ch, fg, bg, attrs });
            }
        }

        let runs = if prev_cells.len() != cells.len() || prev_dims != (bw, bh) {
            vec![DiffRun {
                start: 0,
                cells: cells.clone(),
            }]
        } else {
            compute_runs(&prev_cells, &cells)
        };
        prev_cells.clear();
        prev_cells.extend_from_slice(&cells);
        prev_dims = (bw, bh);
        let frame = Frame {
            seq: frame_seq,
            cols: bw,
            rows: bh,
            cursor_col: cursor.as_ref().map(|p| p.x).unwrap_or(0),
            cursor_row: cursor.as_ref().map(|p| p.y).unwrap_or(0),
            // mixr is always-on-block-cursor (no modal editing); 0 == block.
            cursor_shape: 0,
            cursor_visible: u8::from(cursor.is_some()),
            runs,
        };
        frame_seq = frame_seq.wrapping_add(1);

        {
            let mut w = writer.lock().unwrap();
            if write_message(&mut *w, &Message::Frame(frame)).is_err() {
                return Ok(());
            }
        }

        sleep(Duration::from_millis(POLL_SLEEP_MS)).await;
    }
}

fn modifier_to_bits(m: Modifier) -> u32 {
    let mut a = 0u32;
    if m.contains(Modifier::BOLD) {
        a |= ATTR_BOLD;
    }
    if m.contains(Modifier::DIM) {
        a |= ATTR_DIM;
    }
    if m.contains(Modifier::ITALIC) {
        a |= ATTR_ITALIC;
    }
    if m.contains(Modifier::UNDERLINED) {
        a |= ATTR_UNDERLINE;
    }
    if m.contains(Modifier::REVERSED) {
        a |= ATTR_REVERSED;
    }
    if m.contains(Modifier::CROSSED_OUT) {
        a |= ATTR_CROSSED_OUT;
    }
    a
}

fn color_to_rgba(c: Color, is_bg: bool, palette: Option<HostPalette>) -> u32 {
    // When the host (mnml) has handed us its theme palette, remap
    // mixr's role colors so the panel blends into its container:
    // background fills → host bg, default text → host fg, the green
    // accent → host accent. Semantic colors (red / yellow warnings,
    // cyan focus borders, dim gray) pass through unchanged so they
    // keep their meaning.
    if let Some(p) = palette {
        match c {
            Color::Reset => return if is_bg { p.bg } else { p.fg },
            Color::Black => return p.bg,
            Color::White | Color::Gray => return p.fg,
            Color::Green => return p.accent,
            _ => {}
        }
    }
    match c {
        Color::Rgb(r, g, b) => pack_rgba_u8(r, g, b, 0xff),
        Color::Reset => {
            if is_bg {
                pack_rgba_u8(0x10, 0x11, 0x1c, 0xff)
            } else {
                pack_rgba_u8(0xab, 0xb2, 0xbf, 0xff)
            }
        }
        Color::Black => pack_rgba_u8(0x10, 0x11, 0x1c, 0xff),
        Color::Red => pack_rgba_u8(0xe0, 0x60, 0x60, 0xff),
        Color::Green => pack_rgba_u8(0x84, 0xc8, 0x6f, 0xff),
        Color::Yellow => pack_rgba_u8(0xee, 0xbb, 0x57, 0xff),
        Color::Blue => pack_rgba_u8(0x6e, 0xa2, 0xe7, 0xff),
        Color::Magenta => pack_rgba_u8(0xc9, 0x7a, 0xea, 0xff),
        Color::Cyan => pack_rgba_u8(0x5f, 0xb3, 0xa1, 0xff),
        Color::Gray => pack_rgba_u8(0xab, 0xb2, 0xbf, 0xff),
        Color::DarkGray => pack_rgba_u8(0x42, 0x46, 0x4e, 0xff),
        Color::LightRed => pack_rgba_u8(0xff, 0x82, 0x82, 0xff),
        Color::LightGreen => pack_rgba_u8(0xa6, 0xe2, 0x8c, 0xff),
        Color::LightYellow => pack_rgba_u8(0xff, 0xd7, 0x71, 0xff),
        Color::LightBlue => pack_rgba_u8(0x82, 0xb3, 0xff, 0xff),
        Color::LightMagenta => pack_rgba_u8(0xdc, 0xa5, 0xff, 0xff),
        Color::LightCyan => pack_rgba_u8(0x84, 0xd6, 0xc5, 0xff),
        Color::White => pack_rgba_u8(0xff, 0xff, 0xff, 0xff),
        Color::Indexed(i) => ansi256_to_rgba(i),
    }
}

fn ansi256_to_rgba(i: u8) -> u32 {
    if i < 16 {
        let palette = [
            (0x10, 0x11, 0x1c),
            (0xe0, 0x60, 0x60),
            (0x84, 0xc8, 0x6f),
            (0xee, 0xbb, 0x57),
            (0x6e, 0xa2, 0xe7),
            (0xc9, 0x7a, 0xea),
            (0x5f, 0xb3, 0xa1),
            (0xab, 0xb2, 0xbf),
            (0x42, 0x46, 0x4e),
            (0xff, 0x82, 0x82),
            (0xa6, 0xe2, 0x8c),
            (0xff, 0xd7, 0x71),
            (0x82, 0xb3, 0xff),
            (0xdc, 0xa5, 0xff),
            (0x84, 0xd6, 0xc5),
            (0xff, 0xff, 0xff),
        ];
        let (r, g, b) = palette[i as usize];
        pack_rgba_u8(r, g, b, 0xff)
    } else if i < 232 {
        let n = i - 16;
        let r = (n / 36) * 51;
        let g = ((n / 6) % 6) * 51;
        let b = (n % 6) * 51;
        pack_rgba_u8(r, g, b, 0xff)
    } else {
        let v = 8 + (i - 232) * 10;
        pack_rgba_u8(v, v, v, 0xff)
    }
}

fn unpack_mods(m: u8) -> KeyModifiers {
    let mut out = KeyModifiers::empty();
    if m & MOD_SHIFT != 0 {
        out |= KeyModifiers::SHIFT;
    }
    if m & MOD_CTRL != 0 {
        out |= KeyModifiers::CONTROL;
    }
    if m & MOD_ALT != 0 {
        out |= KeyModifiers::ALT;
    }
    if m & MOD_SUPER != 0 {
        out |= KeyModifiers::SUPER;
    }
    out
}

fn key_to_crossterm(k: &KeyInput) -> KeyEvent {
    let code = match k.code {
        WireKeyCode::Char(c) => CtKeyCode::Char(c),
        WireKeyCode::Backspace => CtKeyCode::Backspace,
        WireKeyCode::Enter => CtKeyCode::Enter,
        WireKeyCode::Left => CtKeyCode::Left,
        WireKeyCode::Right => CtKeyCode::Right,
        WireKeyCode::Up => CtKeyCode::Up,
        WireKeyCode::Down => CtKeyCode::Down,
        WireKeyCode::Home => CtKeyCode::Home,
        WireKeyCode::End => CtKeyCode::End,
        WireKeyCode::PageUp => CtKeyCode::PageUp,
        WireKeyCode::PageDown => CtKeyCode::PageDown,
        WireKeyCode::Tab => CtKeyCode::Tab,
        WireKeyCode::BackTab => CtKeyCode::BackTab,
        WireKeyCode::Delete => CtKeyCode::Delete,
        WireKeyCode::Insert => CtKeyCode::Insert,
        WireKeyCode::Esc => CtKeyCode::Esc,
        WireKeyCode::F(n) => CtKeyCode::F(n),
    };
    KeyEvent {
        code,
        modifiers: unpack_mods(k.mods),
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    }
}

fn mouse_to_crossterm(m: &MouseInput) -> MouseEvent {
    let button = match m.button {
        BUTTON_LEFT => CtMouseButton::Left,
        BUTTON_RIGHT => CtMouseButton::Right,
        BUTTON_MIDDLE => CtMouseButton::Middle,
        _ => CtMouseButton::Left,
    };
    let kind = match m.kind {
        MouseKind::Down => MouseEventKind::Down(button),
        MouseKind::Up => MouseEventKind::Up(button),
        MouseKind::Drag => MouseEventKind::Drag(button),
        MouseKind::Moved => MouseEventKind::Moved,
        MouseKind::ScrollUp => MouseEventKind::ScrollUp,
        MouseKind::ScrollDown => MouseEventKind::ScrollDown,
        MouseKind::ScrollLeft => MouseEventKind::ScrollLeft,
        MouseKind::ScrollRight => MouseEventKind::ScrollRight,
    };
    MouseEvent {
        kind,
        column: m.col,
        row: m.row,
        modifiers: unpack_mods(m.mods),
    }
}

const MERGE_GAP: usize = 4;
const FULL_REPLACE_THRESHOLD: usize = 70;

fn compute_runs(prev: &[WireCell], cur: &[WireCell]) -> Vec<DiffRun> {
    debug_assert_eq!(prev.len(), cur.len());
    let n = cur.len();
    let mut runs: Vec<DiffRun> = Vec::new();
    let mut changed_total = 0usize;
    let mut i = 0;
    while i < n {
        if prev[i] == cur[i] {
            i += 1;
            continue;
        }
        let start = i;
        let mut last_change = i + 1;
        let mut j = i + 1;
        while j < n {
            if prev[j] == cur[j] {
                if j - last_change >= MERGE_GAP {
                    break;
                }
            } else {
                last_change = j + 1;
            }
            j += 1;
        }
        let end = last_change;
        let run_cells: Vec<WireCell> = cur[start..end].to_vec();
        changed_total += run_cells.len();
        runs.push(DiffRun {
            start: start as u32,
            cells: run_cells,
        });
        i = end;
    }
    // If most of the grid changed, the per-run framing is a tax; emit a
    // single full-grid run instead.
    if n > 0 && (changed_total * 100 / n) > FULL_REPLACE_THRESHOLD {
        return vec![DiffRun {
            start: 0,
            cells: cur.to_vec(),
        }];
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_to_rgba_passthrough_without_palette() {
        // No host palette → mixr keeps its own colors.
        assert_eq!(
            color_to_rgba(Color::Rgb(1, 2, 3), false, None),
            pack_rgba_u8(1, 2, 3, 0xff),
        );
        assert_eq!(
            color_to_rgba(Color::Reset, true, None),
            pack_rgba_u8(0x10, 0x11, 0x1c, 0xff),
        );
    }

    #[test]
    fn color_to_rgba_remaps_role_colors_to_host_palette() {
        let p = HostPalette {
            bg: pack_rgba_u8(0x1e, 0x22, 0x2a, 0xff),
            fg: pack_rgba_u8(0xab, 0xb2, 0xbf, 0xff),
            accent: pack_rgba_u8(0x61, 0xaf, 0xef, 0xff),
        };
        let pal = Some(p);
        // Background roles → host bg.
        assert_eq!(color_to_rgba(Color::Reset, true, pal), p.bg);
        assert_eq!(color_to_rgba(Color::Black, true, pal), p.bg);
        // Foreground roles → host fg.
        assert_eq!(color_to_rgba(Color::Reset, false, pal), p.fg);
        assert_eq!(color_to_rgba(Color::White, false, pal), p.fg);
        assert_eq!(color_to_rgba(Color::Gray, false, pal), p.fg);
        // Green accent → host accent.
        assert_eq!(color_to_rgba(Color::Green, false, pal), p.accent);
        // Semantic colors keep their meaning — pass through unchanged.
        assert_eq!(
            color_to_rgba(Color::Red, false, pal),
            pack_rgba_u8(0xe0, 0x60, 0x60, 0xff),
        );
        assert_eq!(
            color_to_rgba(Color::Cyan, false, pal),
            pack_rgba_u8(0x5f, 0xb3, 0xa1, 0xff),
        );
        // Explicit RGB passes straight through regardless of palette.
        assert_eq!(
            color_to_rgba(Color::Rgb(9, 9, 9), false, pal),
            pack_rgba_u8(9, 9, 9, 0xff),
        );
    }
}
