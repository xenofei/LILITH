//! A session: what a successful `core/hello` established for one connection (ARCH-029,
//! ARCH-030). Infrastructure, not a CORE-014 module.

use ww_protocol::{CallerKind, NO_WORKSPACE};

use crate::workspace::WorkspaceRef;

/// One connection's session. Immutable once the hello succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// `s_<16 hex>`.
    pub id: String,
    /// The resolved caller kind, not the declared one.
    pub caller: CallerKind,
    pub non_interactive: bool,
    pub workspace: Option<WorkspaceRef>,
    /// The capabilities both sides offer.
    pub capabilities: Vec<String>,
    /// The client process, from the pipe (0 if Windows did not report it).
    pub client_pid: u32,
    pub profile_id: Option<String>,
}

impl Session {
    /// The workspace id, or `"none"`.
    pub fn workspace_id(&self) -> &str {
        self.workspace.as_ref().map_or(NO_WORKSPACE, |w| &w.id)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test] // spec: ARCH-030
    fn workspace_id_is_the_id_or_none() {
        let mut session = Session {
            id: "s_1".into(),
            caller: CallerKind::Cli,
            non_interactive: false,
            workspace: None,
            capabilities: Vec::new(),
            client_pid: 1,
            profile_id: None,
        };
        assert_eq!(session.workspace_id(), "none");
        session.workspace = Some(WorkspaceRef {
            id: "0123456789abcdef".into(),
            path: PathBuf::from(r"C:\ws"),
        });
        assert_eq!(session.workspace_id(), "0123456789abcdef");
    }
}
