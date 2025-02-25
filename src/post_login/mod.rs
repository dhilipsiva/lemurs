use log::{error, info, warn};
use std::fs;

use users::get_user_groups;

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use crate::auth::utmpx::add_utmpx_entry;
use crate::auth::AuthUserInfo;
use crate::config::Config;
use env_variables::{init_environment, set_xdg_env};

use nix::unistd::{Gid, Uid};

mod env_variables;
mod x;

const INITRCS_FOLDER_PATH: &str = "/etc/lemurs/wms";
const WAYLAND_FOLDER_PATH: &str = "/etc/lemurs/wayland";

#[derive(Clone)]
pub enum PostLoginEnvironment {
    X { xinitrc_path: String },
    Wayland { script_path: String },
    Shell,
}

pub enum EnvironmentStartError {
    WaylandStartError,
    XSetupError(x::XSetupError),
    XStartEnvError(x::XStartEnvError),
    WaitingForEnv,
}

impl PostLoginEnvironment {
    pub fn start<'a>(
        &self,
        config: &Config,
        user_info: &AuthUserInfo<'a>,
    ) -> Result<(), EnvironmentStartError> {
        init_environment(&user_info.name, &user_info.dir, &user_info.shell);
        info!("Set environment variables.");

        set_xdg_env(user_info.uid, &user_info.dir, config.tty);
        info!("Set XDG environment variables");

        match self {
            PostLoginEnvironment::X { xinitrc_path } => {
                x::setup_x(user_info).map_err(EnvironmentStartError::XSetupError)?;
                let mut gui_environment = x::start_env(user_info, xinitrc_path)
                    .map_err(EnvironmentStartError::XStartEnvError)?;

                let pid = gui_environment.id();
                let session = add_utmpx_entry(&user_info.name, config.tty, pid);

                gui_environment.wait().map_err(|err| {
                    warn!("Failed waiting for GUI Environment. Reason: {}", err);
                    EnvironmentStartError::WaitingForEnv
                })?;

                drop(session);
            }
            PostLoginEnvironment::Wayland { script_path } => {
                let uid = user_info.uid;
                let gid = user_info.gid;
                let groups: Vec<Gid> = get_user_groups(&user_info.name, gid)
                    .unwrap()
                    .iter()
                    .map(|group| Gid::from_raw(group.gid()))
                    .collect();

                info!("Starting Wayland Session");
                let Ok(child) = unsafe {
                    Command::new("/bin/sh").pre_exec(move || {
                        // NOTE: The order here is very vital, otherwise permission errors occur
                        // This is basically a copy of how the nightly standard library does it.
                        nix::unistd::setgroups(&groups)
                            .and(nix::unistd::setgid(Gid::from_raw(gid)))
                            .and(nix::unistd::setuid(Uid::from_raw(uid)))
                            .map_err(|err| err.into())
                    })
                }
                .arg("-c")
                .arg(script_path)
                .stdout(Stdio::null()) // TODO: Maybe this should be logged or something?
                .spawn() else {
                    error!("Failed to start Wayland Compositor");
                    return Err(EnvironmentStartError::WaylandStartError);
                };

                info!("Entered Wayland compositor");
                let pid = child.id();

                let session = add_utmpx_entry(&user_info.name, config.tty, pid);

                let Ok(output) = child.wait_with_output() else {
                    error!("Failed to wait on TTY shell, Reason. Returning to Lemurs...");
                    return Ok(());
                };

                drop(session);

                if !output.status.success() {
                    let Ok(output_stderr) = std::str::from_utf8(&output.stderr) else {
                        warn!("Failed to read STDERR output as UTF-8");
                        return Ok(());
                    };

                    if !output_stderr.trim().is_empty() {
                        warn!(
                            "Process came back with: \"\"\"\n{}\n\"\"\"",
                            output_stderr.trim()
                        );
                    }
                }
            }
            PostLoginEnvironment::Shell => {
                let uid = user_info.uid;
                let gid = user_info.gid;
                let groups: Vec<Gid> = get_user_groups(&user_info.name, gid)
                    .unwrap()
                    .iter()
                    .map(|group| Gid::from_raw(group.gid()))
                    .collect();

                info!("Starting TTY shell");
                let shell = &user_info.shell;
                let Ok(child) = unsafe {
                    Command::new(shell).pre_exec(move || {
                        // NOTE: The order here is very vital, otherwise permission errors occur
                        // This is basically a copy of how the nightly standard library does it.
                        nix::unistd::setgroups(&groups)
                            .and(nix::unistd::setgid(Gid::from_raw(gid)))
                            .and(nix::unistd::setuid(Uid::from_raw(uid)))
                            .map_err(|err| err.into())
                    })
                }
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .stdin(Stdio::inherit())
                .spawn() else {
                    error!(
                        "Failed to start TTY shell. Returning to Lemurs...",
                    );
                    return Ok(());
                };

                info!("Entered TTY");
                let pid = child.id();

                let session = add_utmpx_entry(&user_info.name, config.tty, pid);

                let Ok(output) = child.wait_with_output() else {
                    error!("Failed to wait on TTY shell, Reason. Returning to Lemurs...");
                    return Ok(());
                };

                drop(session);

                if !output.status.success() {
                    let Ok(output_stderr) = std::str::from_utf8(&output.stderr) else {
                        warn!("Failed to read STDERR output as UTF-8");
                        return Ok(());
                    };

                    if !output_stderr.trim().is_empty() {
                        warn!(
                            "Process came back with: \"\"\"\n{}\n\"\"\"",
                            output_stderr.trim()
                        );
                    }
                }
            }
        }

        Ok(())
    }
}

pub fn get_envs(with_tty_shell: bool) -> Vec<(String, PostLoginEnvironment)> {
    // NOTE: Maybe we can do something smart with `with_capacity` here.
    let mut envs = Vec::new();

    match fs::read_dir(INITRCS_FOLDER_PATH) {
        Ok(paths) => {
            for path in paths {
                if let Ok(path) = path {
                    let file_name = path.file_name().into_string();

                    if let Ok(file_name) = file_name {
                        if let Ok(metadata) = path.metadata() {
                            if std::os::unix::fs::MetadataExt::mode(&metadata) & 0o111 == 0 {
                                warn!(
                            "'{}' is not executable and therefore not added as an environment",
                            file_name
                        );

                                continue;
                            }
                        }

                        envs.push((
                            file_name,
                            PostLoginEnvironment::X {
                                xinitrc_path: match path.path().to_str() {
                                    Some(p) => p.to_string(),
                                    None => {
                                        warn!(
                                    "Skipped item because it was impossible to convert to string"
                                );
                                        continue;
                                    }
                                },
                            },
                        ));
                    } else {
                        warn!("Unable to convert OSString to String");
                    }
                } else {
                    warn!("Ignored errorinous path: '{}'", path.unwrap_err());
                }
            }
        }
        Err(_) => {
            warn!("Failed to read from the X folder '{}'", INITRCS_FOLDER_PATH);
        }
    }

    match fs::read_dir(WAYLAND_FOLDER_PATH) {
        Ok(paths) => {
            for path in paths {
                if let Ok(path) = path {
                    let file_name = path.file_name().into_string();

                    if let Ok(file_name) = file_name {
                        if let Ok(metadata) = path.metadata() {
                            if std::os::unix::fs::MetadataExt::mode(&metadata) & 0o111 == 0 {
                                warn!(
                            "'{}' is not executable and therefore not added as an environment",
                            file_name
                        );

                                continue;
                            }
                        }

                        envs.push((
                            file_name,
                            PostLoginEnvironment::Wayland {
                                script_path: match path.path().to_str() {
                                    Some(p) => p.to_string(),
                                    None => {
                                        warn!(
                                    "Skipped item because it was impossible to convert to string"
                                );
                                        continue;
                                    }
                                },
                            },
                        ));
                    } else {
                        warn!("Unable to convert OSString to String");
                    }
                } else {
                    warn!("Ignored errorinous path: '{}'", path.unwrap_err());
                }
            }
        }
        Err(_) => {
            warn!(
                "Failed to read from the wayland folder '{}'",
                WAYLAND_FOLDER_PATH
            );
        }
    }

    if envs.is_empty() || with_tty_shell {
        envs.push(("TTYSHELL".to_string(), PostLoginEnvironment::Shell));
    }

    envs
}
