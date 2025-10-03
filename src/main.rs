use log::{debug, error, info, warn};

use sd_notify;
use std::fs;
use std::io;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::os::unix::{fs::FileTypeExt, net::UnixListener};
use std::path;
use wayland_client::{
    Connection, QueueHandle, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::wl_registry,
};
use wayland_protocols::wp::security_context::v1::client::{
    wp_security_context_manager_v1, wp_security_context_v1,
};
use xdg;
use nix::sys::{
    wait::{waitpid, WaitPidFlag},
    signalfd::{SignalFd, SigSet, SfdFlags},
    signal::Signal,
    signal::Signal::*,
};
use env_logger::Env;
use clap::Parser;

/// Set up a Wayland socket with an attached security context
///
/// See https://wayland.app/protocols/security-context-v1
#[derive(Parser, Debug)]
#[command(version, about, long_about)]
struct Cli {
    #[arg(long, env = "WLSCTX_APP_ID")]
    app_id: String,
    /// Instance ID
    #[arg(long, env = "WLSCTX_INSTANCE_ID")]
    instance_id: String,
    /// Sandbox engine ID
    #[arg(long, env = "WLSCTX_SANDBOX_ENGINE")]
    sandbox_engine: String,
    /// Derive app_id and instance_id from systemd unit name (app-id@instance.service)
    #[arg(long, env)]
    systemd_unit: Option<String>,
    /// Receive socket via systemd socket activation (LISTEN_FDS)
    #[arg(long, group = "socket", env = "LISTEN_FDS")]
    socket_activation: bool,
    /// Listen on Unix socket
    #[arg(long, group = "socket", env = "WLSCTX_SOCKET_PATH")]
    listen: Option<path::PathBuf>,
}

struct State;

impl wayland_client::Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        // This mutex contains an up-to-date list of the currently known globals
        // including the one that was just added or destroyed
        _data: &GlobalListContents,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        /* react to dynamic global events here */
    }
}

// The security context protocol has no events that we need to manage
delegate_noop!(State: wp_security_context_manager_v1::WpSecurityContextManagerV1);
delegate_noop!(State: wp_security_context_v1::WpSecurityContextV1);

// The main function of our program
fn main() {
    let env = Env::default().default_filter_or("warn");
    env_logger::init_from_env(env);
    let cli = Cli::parse();

    let listener = match (cli.socket_activation, cli.listen) {
        (true, None) => match sd_notify::listen_fds().map(|mut fds| fds.next()) {
            Ok(Some(raw_fd)) => {
                // SAFETY: sd_notify::listen_fds() unsets the LISTEN_FDS variable so we should be the
                // only user of this fd
                info!("Received socket activation environment from parent {raw_fd:#?}");
                unsafe { UnixListener::from_raw_fd(raw_fd) }
            }
            _ => {
                panic!("Failed to get socket FD from activation environment")
            }
        },
        (false, Some(socket_path)) => {
            let socket_abspath = match socket_path.is_absolute() {
                true => socket_path,
                false => xdg::BaseDirectories::new()
                    .place_runtime_file(socket_path)
                    .unwrap(),
            };
            let _ = match fs::metadata(&socket_abspath) {
                Ok(meta) => {
                    if meta.file_type().is_socket() {
                        info!("Removing old socket {socket_abspath:?}");
                        let _ = fs::remove_file(&socket_abspath)
                            .inspect_err(|e| error!("Failed to remove existing socket: {e}"));
                    } else {
                        error!("Path already exists and is not a socket {socket_abspath:?}");
                    }
                }
                _ => (),
            };
            UnixListener::bind(socket_abspath).expect("Failed to bind to Unix socket")
        }
        _ => {
            panic!("No listening socket provided")
        }
    };
    let _ = match listener.local_addr() {
        Ok(local_addr) => info!("Listening on {local_addr:?}"),
        _ => (),
    };

    let close_fd: OwnedFd = {
        // Create a Wayland connection by connecting to the server through the
        // environment-provided configuration.
        let conn = Connection::connect_to_env().expect("upstream Wayland connection failed");
        let (globals, mut event_queue) = registry_queue_init::<State>(&conn).unwrap();
        let qh = &event_queue.handle();
        let security_context_manager: wp_security_context_manager_v1::WpSecurityContextManagerV1 =
            globals.bind(qh, 1..=1, ()).unwrap();
        let (reader, writer) = io::pipe().unwrap();
        let security_context =
            security_context_manager.create_listener(listener.as_fd(), reader.as_fd(), qh, ());
        security_context_manager.destroy();
        let sandbox_engine = &cli.sandbox_engine;
        let app_id = &cli.app_id;
        let instance_id = &cli.instance_id;
        info!("Create security context mapping for {app_id} ({instance_id})");
        security_context.set_sandbox_engine(sandbox_engine.clone());
        security_context.set_app_id(app_id.clone());
        security_context.set_instance_id(instance_id.clone());
        security_context.commit();
        security_context.destroy();
        event_queue.roundtrip(&mut State {}).unwrap();
        writer.into()
    };
    info!("Holding close_fd open to keep the tagged wayland socket available {close_fd:?}");
    let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);
    let mut mask = SigSet::all();
    for sig in (SIGFPE | SIGILL | SIGSEGV | SIGBUS | SIGABRT | SIGTRAP | SIGSYS).iter() {
        mask.remove(sig);
    }
    mask.thread_block().unwrap();
    let sigfd = SignalFd::with_flags(&mask, SfdFlags::SFD_CLOEXEC).unwrap();
    while let Some(siginfo) = sigfd.read_signal().unwrap() {
        debug!("Signal: {siginfo:?}");
        match Signal::try_from(siginfo.ssi_signo as i32).unwrap() {
            SIGTERM | SIGINT => {
                debug!("Stopping");
                break;
            }
            SIGHUP => {
                warn!("TODO: SIGHUP restart");
                break;
            }
            SIGCHLD => {
                debug!("reap zombies");
                while let Ok(status) = waitpid(None, Some(WaitPidFlag::WNOHANG)) {
                    debug!("status: {status:?}");
                }
            }
            SIGTSTP | SIGTTOU | SIGTTIN => {
                debug!("ignoring kernel attempting to stop us: tty has TOSTOP set");
            }
            sig => {
                warn!("Unexpected signal {sig:#?}");
            }
        }
    }
    info!("Shutting down...");
}
