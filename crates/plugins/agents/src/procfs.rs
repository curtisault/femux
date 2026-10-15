//! Process state for tier 2 (REQ-AGT-002), Linux only: `/proc/<pid>/stat` and `wchan`.

/// A process's scheduler state letter (`R` running, `S` sleeping, ...) and the kernel function
/// it waits in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcState {
    pub state: char,
    pub wchan: String,
}

/// `/proc/<pid>/stat`'s state field (after the parenthesised command, which may hold spaces).
pub fn parse_stat(stat: &str) -> Option<char> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().next()?.chars().next()
}

pub fn read(pid: u32) -> Option<ProcState> {
    if !cfg!(target_os = "linux") || pid == 0 {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let wchan = std::fs::read_to_string(format!("/proc/{pid}/wchan")).unwrap_or_default();
    Some(ProcState {
        state: parse_stat(&stat)?,
        wchan: wchan.trim().to_owned(),
    })
}

/// Whether the process sleeps in a terminal read: it waits for a line from the user, and
/// that alone means `blocked` (REQ-AGT-002). Recent kernels report `n_tty_read`'s sleep as
/// `wait_woken`.
pub fn tty_read(p: &ProcState) -> bool {
    p.state == 'S'
        && ["tty_read", "wait_woken"]
            .iter()
            .any(|w| p.wchan.contains(w))
}

/// Whether the process sleeps where an event loop parks (poll, select, epoll, a pipe): it
/// waits for something, a prompt, an answer or the network alike, so the screen decides
/// (REQ-AGT-002, 0.11.0).
pub fn event_wait(p: &ProcState) -> bool {
    p.state == 'S'
        && ["poll", "select", "epoll", "pipe_read"]
            .iter()
            .any(|w| p.wchan.contains(w))
}

/// Whether the process sleeps waiting for input of any kind.
pub fn waiting_for_input(p: &ProcState) -> bool {
    tty_read(p) || event_wait(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_state_after_the_command_name() {
        assert_eq!(parse_stat("4242 (claude code) S 1 4242 ..."), Some('S'));
        assert_eq!(parse_stat("7 (a) b) R 1"), Some('R'));
        assert_eq!(parse_stat("nonsense"), None);
        let s = |state, wchan: &str| ProcState {
            state,
            wchan: wchan.into(),
        };
        assert!(waiting_for_input(&s('S', "n_tty_read")));
        assert!(waiting_for_input(&s('S', "do_epoll_wait")));
        // 0.11.0: only a terminal read is blocked by itself; an event loop's wait is not.
        assert!(tty_read(&s('S', "n_tty_read")) && tty_read(&s('S', "wait_woken")));
        assert!(!tty_read(&s('S', "do_epoll_wait")) && event_wait(&s('S', "do_epoll_wait")));
        assert!(event_wait(&s('S', "do_select")) && event_wait(&s('S', "anon_pipe_read.cold")));
        assert!(!event_wait(&s('S', "n_tty_read")) && !event_wait(&s('R', "do_epoll_wait")));
        assert!(!waiting_for_input(&s('S', "hrtimer_nanosleep")));
        assert!(!waiting_for_input(&s('R', "n_tty_read")));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn reads_this_process() {
        let p = read(std::process::id()).unwrap();
        assert!("RSD".contains(p.state), "{p:?}");
    }
}
