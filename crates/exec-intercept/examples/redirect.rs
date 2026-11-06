use needy_exec_intercept::{Action, Command, Replacement, supervise};
use std::sync::atomic::AtomicBool;

fn main() -> std::io::Result<()> {
    let mut root = Command::new("/bin/sh")?;
    root.args = vec!["-c".into(), "/usr/bin/printf 'original\\n'".into()];
    let report = supervise(root, &AtomicBool::new(false), |exec| {
        if std::path::Path::new(&exec.executable).file_name() == Some("printf".as_ref()) {
            Ok(Action::Redirect(Replacement {
                executable: "/bin/echo".into(),
                argv: vec!["echo".into(), "redirected".into()],
                environment: None,
            }))
        } else {
            Ok(Action::Continue)
        }
    })?;
    eprintln!("{report:?}");
    std::process::exit(report.outcome.shell_code());
}
