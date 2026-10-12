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

/// Whether the process sleeps waiting for input: a terminal read, or the poll/select/epoll
/// wait an event loop parks in while nothing happens.
pub fn waiting_for_input(p: &ProcState) -> bool {
    p.state == 'S'
        && [
            "tty_read",
            "wait_woken",
            "poll",
            "select",
            "epoll",
            "pipe_read",
            "do_sys_poll",
        ]
        .iter()
        .any(|w| p.wchan.contains(w))
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
