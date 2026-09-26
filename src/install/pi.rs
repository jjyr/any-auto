use crate::config;
use std::path::PathBuf;

pub fn is_detected() -> bool {
    super::executable("pi")
}

pub fn is_installed() -> bool {
    pi_extension().is_file()
}

pub fn pi_extension() -> PathBuf {
    std::env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| config::home().join(".pi/agent"))
        .join("extensions/any-auto.ts")
}
