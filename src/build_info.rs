pub(crate) const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "\ncommit: ",
    env!("RALPH_GIT_COMMIT"),
    "\nstate: ",
    env!("RALPH_GIT_STATE")
);

pub(crate) fn display() -> String {
    format!(
        "ralph {} (commit {}, {})",
        env!("CARGO_PKG_VERSION"),
        env!("RALPH_GIT_COMMIT"),
        env!("RALPH_GIT_STATE")
    )
}
