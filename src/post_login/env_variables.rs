use log::{error, info};
use std::env;

fn env_set_and_announce(key: &str, value: &str) {
    env::set_var(key, value);
    info!("Set environment variable '{}' to '{}'", key, value);
}

fn get_dotfiles_dir(homedir: &str) -> String {
    format!("{homedir}/.files")
}

/// Set all the environment variables
pub fn init_environment(username: &str, homedir: &str, _shell: &str) {
    env_set_and_announce("HOME", homedir);
    let pwd = homedir;
    if env::set_current_dir(pwd).is_ok() {
        info!("Successfully changed working directory to {}!", pwd);
    } else {
        error!("Failed to change the working directory to {}", pwd);
    }

    env_set_and_announce("USER", username);
    env_set_and_announce("LOGNAME", username);
    env_set_and_announce("PATH", &format!("{homedir}/bin:{homedir}/bin/firefox:{homedir}/.cargo/bin:/home/linuxbrew/.linuxbrew/bin:/home/linuxbrew/.linuxbrew/sbin:/usr/local/sbin:/usr/local/bin:/usr/bin"));
    env_set_and_announce("EDITOR", "hx");
    env_set_and_announce("SHELL", &format!("{homedir}/.cargo/bin/nu"));

    let dotfiles_dir = get_dotfiles_dir(homedir);
    env_set_and_announce("DOTFILES_DIR", dotfiles_dir.as_str());
    let user_bin = format!("{}/bin", homedir);
    env_set_and_announce("USER_BIN", &user_bin);

    env_set_and_announce("PIPENV_VENV_IN_PROJECT", "1");
    env_set_and_announce("PYTHONBREAKPOINT", "ipdb.set_trace");
    env_set_and_announce("STARSHIP_CONFIG", &format!("{dotfiles_dir}/starship.toml"));

    // env::set_var("MAIL", "..."); TODO: Add
}

// NOTE: This uid: u32 might be better set to libc::uid_t
/// Set the XDG environment variables
pub fn set_xdg_env(uid: u32, homedir: &str, tty: u8) {
    // This is according to https://wiki.archlinux.org/title/XDG_Base_Directory
    let dotfiles_dir = get_dotfiles_dir(homedir);
    let xdg_config_dir = format!("{}/.config", dotfiles_dir);
    env_set_and_announce("XDG_CONFIG_DIR", &xdg_config_dir);
    env_set_and_announce("XDG_CONFIG_HOME", &xdg_config_dir);
    env_set_and_announce("XDG_CACHE_HOME", &format!("{}/.cache", homedir));
    env_set_and_announce("XDG_DATA_HOME", &format!("{}/.local/share", homedir));
    env_set_and_announce("XDG_STATE_HOME", &format!("{}/.local/state", homedir));
    env_set_and_announce(
        "XDG_DATA_DIRS",
        "/home/linuxbrew/.linuxbrew/share:/usr/local/share:/usr/share",
    );
    env_set_and_announce("XDG_CONFIG_DIRS", "/etc/xdg");

    env_set_and_announce("ZELLIJ_CONFIG_DIR", &format!("{xdg_config_dir}/zellij"));
    env_set_and_announce("INPUTRC", &format!("{xdg_config_dir}/readline/inputrc"));
    env_set_and_announce("XDG_RUNTIME_DIR", &format!("/run/user/{}", uid));
    env_set_and_announce("XDG_SESSION_DIR", "user");
    env_set_and_announce("XDG_SESSION_ID", "1");
    env_set_and_announce("XDG_SEAT", "seat0");
    env_set_and_announce("XDG_VTNR", &tty.to_string());
}
