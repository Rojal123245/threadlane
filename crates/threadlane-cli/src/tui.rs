use crossterm::{
    cursor::Show,
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{self, stdout, Stdout};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;
type PanicHook = Arc<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync + 'static>;

pub struct TerminalGuard {
    terminal: Tui,
    cleanup: Arc<Mutex<CleanupState>>,
    previous_panic_hook: Option<PanicHook>,
}

impl TerminalGuard {
    pub fn restore(&mut self) -> io::Result<()> {
        self.cleanup.lock().expect("terminal cleanup lock poisoned").restore()
    }
}

impl Deref for TerminalGuard {
    type Target = Tui;

    fn deref(&self) -> &Self::Target { &self.terminal }
}

impl DerefMut for TerminalGuard {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.terminal }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
        if !std::thread::panicking() {
            if let Some(previous_hook) = self.previous_panic_hook.take() {
                std::panic::set_hook(Box::new(move |panic_info| previous_hook(panic_info)));
            }
        }
    }
}

pub struct CleanupState {
    raw_mode: bool,
    alternate_screen: bool,
    mouse_capture: bool,
    cursor_hidden: bool,
    restored: bool,
    #[cfg(test)]
    test_mode: bool,
}

impl CleanupState {
    fn new() -> Self {
        Self {
            raw_mode: false,
            alternate_screen: false,
            mouse_capture: false,
            cursor_hidden: false,
            restored: false,
            #[cfg(test)]
            test_mode: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_test() -> Self {
        Self { test_mode: true, ..Self::new() }
    }

    pub fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }

        #[cfg(test)]
        if self.test_mode {
            self.restored = true;
            return Ok(());
        }

        let mut first_error = None;
        if self.raw_mode {
            match disable_raw_mode() {
                Ok(()) => self.raw_mode = false,
                Err(error) => first_error = Some(error),
            }
        }
        if self.alternate_screen {
            match execute!(stdout(), LeaveAlternateScreen) {
                Ok(()) => self.alternate_screen = false,
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        if self.mouse_capture {
            match execute!(stdout(), DisableMouseCapture) {
                Ok(()) => self.mouse_capture = false,
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        if self.cursor_hidden {
            match execute!(stdout(), Show) {
                Ok(()) => self.cursor_hidden = false,
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }

        if let Some(error) = first_error {
            Err(error)
        } else {
            self.restored = true;
            Ok(())
        }
    }

    #[cfg(test)]
    pub(crate) fn is_restored(&self) -> bool { self.restored }
}

/// Initializes terminal raw mode and alternate screen buffer.
pub fn init() -> io::Result<TerminalGuard> {
    let mut cleanup = CleanupState::new();

    enable_raw_mode()?;
    cleanup.raw_mode = true;
    if let Err(error) = execute!(stdout(), EnterAlternateScreen) {
        let _ = cleanup.restore();
        return Err(error);
    }
    cleanup.alternate_screen = true;

    if let Err(error) = execute!(stdout(), EnableMouseCapture) {
        let _ = cleanup.restore();
        return Err(error);
    }
    cleanup.mouse_capture = true;

    let mut terminal = match Terminal::new(CrosstermBackend::new(stdout())) {
        Ok(terminal) => terminal,
        Err(error) => {
            let _ = cleanup.restore();
            return Err(error);
        }
    };
    if let Err(error) = terminal.hide_cursor() {
        let _ = cleanup.restore();
        return Err(error);
    }
    cleanup.cursor_hidden = true;
    if let Err(error) = terminal.clear() {
        let _ = cleanup.restore();
        return Err(error);
    }

    let cleanup = Arc::new(Mutex::new(cleanup));
    let panic_cleanup = Arc::clone(&cleanup);
    let previous_panic_hook: PanicHook = Arc::from(std::panic::take_hook());
    let panic_hook_for_handler = Arc::clone(&previous_panic_hook);
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = panic_cleanup.lock().map_err(|_| ()).and_then(|mut cleanup| cleanup.restore().map_err(|_| ()));
        panic_hook_for_handler(panic_info);
    }));

    Ok(TerminalGuard { terminal, cleanup, previous_panic_hook: Some(previous_panic_hook) })
}

/// Restores terminal state back to original mode for callers without a guard.
#[allow(dead_code)]
pub fn restore() -> io::Result<()> {
    let mut cleanup = CleanupState::new();
    cleanup.raw_mode = true;
    cleanup.alternate_screen = true;
    cleanup.mouse_capture = true;
    cleanup.cursor_hidden = true;
    cleanup.restore()
}

#[cfg(test)]
mod tests {
    use super::CleanupState;

    #[test]
    fn terminal_cleanup_is_idempotent() {
        let mut cleanup = CleanupState::new_for_test();
        cleanup.restore().unwrap();
        cleanup.restore().unwrap();
        assert!(cleanup.is_restored());
    }
}
