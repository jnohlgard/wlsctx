use log::{debug, error, info};

use wayland_client::{
    delegate_noop,
    protocol::{
        wl_registry,
    },
    Connection, Dispatch, QueueHandle
};
use wayland_protocols::wp::security_context::v1::client::{
    wp_security_context_manager_v1,
    wp_security_context_v1,
};
use xdg;
use std::os::unix::{
    fs::FileTypeExt,
    net::UnixListener,
};
use std::os::fd::{
    AsFd,
    OwnedFd,
    FromRawFd,
};
use std::io;
use std::fs;
use std::path;
use sd_notify;

extern crate pretty_env_logger;

use clap::Parser;

/// Set up a Wayland socket with an attached security context
///
/// See https://wayland.app/protocols/security-context-v1
#[derive(Parser, Debug)]
#[command(version, about, long_about)]
struct Cli {
    #[arg(long)]
    app_id: String,
    #[arg(long)]
    /// Instance ID
    instance_id: String,
    /// Sandbox engine ID
    #[arg(long)]
    sandbox_engine: String,
    /// Receive socket via systemd socket activation (LISTEN_FDS)
    #[arg(long, group = "socket")]
    socket_activation: bool,
    /// Listen on Unix socket
    #[arg(long, group = "socket")]
    listen: Option<path::PathBuf>,
}

// This struct represents the state of our app. This simple app does not
// need any state, by this type still supports the `Dispatch` implementations.
#[derive(Debug)]
struct State {
    app_id: String,
    instance_id: String,
    sandbox_engine: String,
    listen_fd: OwnedFd,
    close_fd: Option<OwnedFd>,
    needs_roundtrip: bool,
}

// Implement `Dispatch<WlRegistry, ()> for out state. This provides the logic
// to be able to process events for the wl_registry interface.
//
// The second type parameter is the user-data of our implementation. It is a
// mechanism that allows you to associate a value to each particular Wayland
// object, and allow different dispatching logic depending on the type of the
// associated value.
//
// In this example, we just use () as we don't have any value to associate. See
// the `Dispatch` documentation for more details about this.
impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _userdata: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        // When receiving events from the wl_registry, we are only interested in the
        // `global` event, which signals a new available global.
        if let wl_registry::Event::Global { name, interface, version } = event {
            match &interface[..] {
                "wp_security_context_manager_v1" => {
                    debug!("Wayland security context manager protocol {interface} (v{version})");
                    let security_context_manager = registry.bind::<wp_security_context_manager_v1::WpSecurityContextManagerV1, _, _>(name, wp_security_context_manager_v1::REQ_CREATE_LISTENER_SINCE, qh, ());
                    let (reader, writer) = io::pipe().unwrap();
                    let security_context = security_context_manager.create_listener(state.listen_fd.as_fd(), reader.as_fd(), qh, ());
                    state.close_fd = Some(writer.into());
                    let app_id = &state.app_id;
                    let instance_id = &state.instance_id;
                    info!("Create security context mapping for {app_id} ({instance_id})");
                    security_context.set_sandbox_engine(state.sandbox_engine.clone());
                    security_context.set_app_id(app_id.clone());
                    security_context.set_instance_id(instance_id.clone());
                    security_context.commit();
                    security_context.destroy();
                    state.needs_roundtrip = true;
                },
                _ => {
                    debug!("[{name}] {interface} (v{version})");
                }
            }
        }
    }
}

// The security context protocol has no events that we need to manage
delegate_noop!(State: ignore wp_security_context_manager_v1::WpSecurityContextManagerV1);
delegate_noop!(State: ignore wp_security_context_v1::WpSecurityContextV1);

// The main function of our program
fn main() {
    pretty_env_logger::init();
    let cli = Cli::parse();

    let listener = match (cli.socket_activation, cli.listen) {
        (true, None) => match sd_notify::listen_fds().map(|mut fds| fds.next()) {
            Ok(Some(raw_fd)) => {
                // SAFETY: sd_notify::listen_fds() unsets the LISTEN_FDS variable so we should be the
                // only user of this fd
                info!("Received socket activation environment from parent {raw_fd:#?}");
                unsafe { UnixListener::from_raw_fd(raw_fd) }
            },
            _ => {
                panic!("Failed to get socket FD from activation environment")
            }
        },
        (false, Some(socket_path)) => {
            let socket_abspath = match socket_path.is_absolute() {
                true => socket_path,
                false => xdg::BaseDirectories::new().place_runtime_file(socket_path).unwrap(),
            };
            let _ = match fs::metadata(&socket_abspath) { 
                Ok(meta) => {
                    if meta.file_type().is_socket() {
                        info!("Removing old socket {socket_abspath:?}");
                        let _ = fs::remove_file(&socket_abspath)
                            .inspect_err(|e| error!("Failed to remove existing socket: {e}"));
                    }
                    else {
                        error!("Path already exists and is not a socket {socket_abspath:?}");
                    }
                },
                _ => (),
            };
            UnixListener::bind(socket_abspath).expect("Failed to bind to Unix socket")
        },
        _ => {
            panic!("No listening socket provided")
        },
    };
    let _ = match listener.local_addr() {
        Ok(local_addr) => info!("Listening on {local_addr:?}"),
        _ => (),
    };


    let mut state = State{
        sandbox_engine: cli.sandbox_engine,
        app_id: cli.app_id,
        instance_id: cli.instance_id,
        listen_fd: listener.into(),
        close_fd: None,
        needs_roundtrip: true,
    };
    // Create a Wayland connection by connecting to the server through the
    // environment-provided configuration.
    let conn = Connection::connect_to_env().expect("upstream Wayland connection failed");

    // Retrieve the WlDisplay Wayland object from the connection. This object is
    // the starting point of any Wayland program, from which all other objects will
    // be created.
    let display = conn.display();

    // Create an event queue for our event processing
    let mut event_queue = conn.new_event_queue();
    // And get its handle to associated new objects to it
    let qh = event_queue.handle();

    // Create a wl_registry object by sending the wl_display.get_registry request
    // This method takes two arguments: a handle to the queue the newly created
    // wl_registry will be assigned to, and the user-data that should be associated
    // with this registry (here it is () as we don't need user-data).
    let _registry = display.get_registry(&qh, ());

    // At this point everything is ready, and we just need to wait to receive the events
    // from the wl_registry.

    // To actually receive the events, we invoke the `sync_roundtrip` method. This method
    // is special and you will generally only invoke it during the setup of your program:
    // it will block until the server has received and processed all the messages you've
    // sent up to now.
    //
    // In our case, that means it'll block until the server has received our
    // wl_display.get_registry request, and as a reaction has sent us a batch of
    // wl_registry.global events.
    //
    // `sync_roundtrip` will then empty the internal buffer of the queue it has been invoked
    // on, and thus invoke our `Dispatch` implementation that prints the list of advertized
    // globals.
    while state.needs_roundtrip {
        state.needs_roundtrip = false;
        event_queue.roundtrip(&mut state).unwrap();
    }
    match &state.close_fd {
        Some(fd) => {
            println!("Listen fd: {fd:?}");
        },
        _ => {
            println!("No listen_fd");
        },
    }
}
