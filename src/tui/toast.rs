use std::time::Instant;

pub struct Toast {
    message: Option<String>,
    expiry: Option<Instant>,
}

impl Toast {
    pub fn new() -> Self {
        Self {
            message: None,
            expiry: None,
        }
    }

    pub fn show(&mut self, text: &str, duration_secs: f64) {
        self.message = Some(text.to_string());
        self.expiry = Some(Instant::now() + std::time::Duration::from_secs_f64(duration_secs));
    }

    pub fn current(&mut self) -> Option<String> {
        if let Some(expiry) = self.expiry
            && Instant::now() < expiry
        {
            return self.message.clone();
        }
        self.message = None;
        self.expiry = None;
        None
    }

    /// Read the active toast without expiring it. Used by IPC status
    /// writers so external scripts (smoke tests, dashboards) can observe
    /// toast feedback for actions they trigger.
    pub fn peek(&self) -> Option<&str> {
        let expiry = self.expiry?;
        if Instant::now() < expiry {
            self.message.as_deref()
        } else {
            None
        }
    }
}
