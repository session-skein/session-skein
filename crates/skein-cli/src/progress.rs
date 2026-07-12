use std::io::IsTerminal;

pub(crate) struct Progress {
    enabled: bool,
}

impl Progress {
    pub(crate) fn cli(force_json: bool) -> Self {
        Self {
            enabled: should_emit(
                force_json || crate::output::is_json(),
                std::io::stderr().is_terminal(),
            ),
        }
    }

    pub(crate) fn stage(&self, message: &str) {
        if self.enabled {
            eprintln!("[skein] {message}");
        }
    }
}

fn should_emit(json: bool, stderr_is_terminal: bool) -> bool {
    !json && stderr_is_terminal
}

#[cfg(test)]
mod tests {
    use super::should_emit;

    #[test]
    fn progress_requires_human_output_and_an_interactive_stderr() {
        assert!(should_emit(false, true));
        assert!(!should_emit(true, true));
        assert!(!should_emit(false, false));
        assert!(!should_emit(true, false));
    }
}
