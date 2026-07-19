#[cfg(target_os = "linux")]
include!("linux.rs");

#[cfg(target_os = "macos")]
include!("macos.rs");

#[cfg(target_os = "windows")]
include!("windows.rs");

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!("rusty_v8 bindings are not bundled for this operating system");
