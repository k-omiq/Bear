//! Child processes the windowed app starts.

use std::ffi::OsStr;
use std::process::Command;

/// A `Command` for a console program (Python, uv, a helper, `nvidia-smi`).
///
/// On Windows a GUI process that starts a console program without
/// `CREATE_NO_WINDOW` gets a console window for it, which shows up as an
/// empty black window and kills the child when someone closes it. With the
/// flag the child gets a console with no window, and every console program it
/// starts in turn (a venv launcher's real interpreter, a script's own
/// `subprocess.run`) shares that hidden console instead of opening one.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
