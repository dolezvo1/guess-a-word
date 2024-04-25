
use std::collections::HashMap;
use std::env;
use std::net::TcpListener;
use std::sync::{Arc, mpsc, RwLock};

#[cfg(target_family="unix")]
use std::os::unix::net::UnixListener;

mod protocol;
mod server_worker;
mod util;

use crate::protocol::{ClientState, GuessProtocol as Ptcl, ProtocolReader, ProtocolWriter};
use crate::server_worker::ServerWorker;
use crate::util::{parse_arg, spawn_network_listener, InternalMessage as IMsg};

pub type Sender = mpsc::Sender<IMsg<Ptcl>>;
pub struct ServerState {
    id_generator: usize,
    available_clients: HashMap<String, (ClientState, Sender)>,
}
pub type Store = Arc<RwLock<ServerState>>;

static ACCEPTED_OPTIONS: [&str; 3] = [
    "--server-password",
    "--tcp-port",
    "--unix-socket-path",
];

#[tokio::main]
async fn main() -> std::io::Result<()> {
    
    // Validate arguments
    let args: Vec<_> = env::args().collect();
    let password = parse_arg::<String>("--server-password", &args, &ACCEPTED_OPTIONS)
                        .expect("Server password must be provided (`--server-password`)");
    
    let (tcp_port, unix_socket_path)
        = (parse_arg::<u16>("--tcp-port", &args, &ACCEPTED_OPTIONS),
           parse_arg::<String>("--unix-socket-path", &args, &ACCEPTED_OPTIONS));
    if tcp_port.is_none() && unix_socket_path.is_none() {
        eprintln!("At least one listener option must be provided (`--tcp-port PORT`, `--unix-socket-path PATH`)");
        return Err(std::io::Error::other("invalid arguments"));
    }
    
    // Initialize main server storage
    let store = Arc::new(RwLock::new(
        ServerState {
            id_generator: 1,
            available_clients: HashMap::new(),
        }));
    
    // Establish channel for client stream dispatch
    let (tx, receiver) = mpsc::channel::<(Box<dyn ProtocolReader<Ptcl> + Send>,
                                          Box<dyn ProtocolWriter<Ptcl> + Send>)>();
    #[cfg_attr(not(target_family="unix"), allow(unused_variables))]
    let (tx_tcp, tx_unix_socket) = (tx.clone(), tx);
    
    // Create TCP listener thread that sends client stream down the channel
    if let Some(tcp_port) = tcp_port {
        tokio::spawn(async move {
            let tcp_listener = TcpListener::bind(("127.0.0.1", tcp_port))
            	.expect("TCP listener could not be started");
            
            for stream in tcp_listener.incoming() {
                let _ = stream.map(|stream| stream.try_clone().map(|clone| {
                    let _ = tx_tcp.send((Box::new(stream), Box::new(clone)));
                }));
            }
            unreachable!();
        });
    }
    
    // Create Unix socket listener that sends client stream down the channel
    #[cfg(target_family="unix")]
    if let Some(unix_socket_path) = unix_socket_path {
        tokio::spawn(async move {
            // Clean up the socket if it already exists
            if std::fs::metadata(&unix_socket_path).is_ok() {
                std::fs::remove_file(&unix_socket_path)
                	.expect("Unix Socket already exists and could not be cleaned up");
            }

            // Create a Unix listener on the socket path
            let listener = UnixListener::bind(&unix_socket_path)
            			.expect("Unix Socket listener could not be started");
            
            // Loop over incoming connections
            for stream in listener.incoming() {
            	let _ = stream.map(|stream| stream.try_clone().map(|clone| {
                    let _ = tx_unix_socket.send((Box::new(stream), Box::new(clone)));
                }));
            }
            unreachable!();
        });
    }
    
    // Start a communication task for each connection received through the channel
    loop {
        if let Ok(mut client) = receiver.recv() {
            let (password, store) = (password.clone(), store.clone());
            tokio::spawn(async move {
                
                // Establish joint message channel
                let (tx, joint_rx) = mpsc::channel::<IMsg<Ptcl>>();
                
                // Establish connection to client
                if let Ok(mut sw) = ServerWorker::new(
                                            (client.0.as_mut(), client.1.as_mut()),
                                            password, store, tx.clone()) {
                    // Add network listener to the joint channel
                    let _ = spawn_network_listener(client.0, tx.clone());
                    
                    // "At this moment, the server answers to any requests the client sends to the server. For unknown requests, the server must respond as well, such that client can identify it as an error."
                    while let Ok(msg) = joint_rx.recv() {
                        if sw.handle_message(msg, client.1.as_mut()).is_err() {
                            break;
                        }
                    }
                }
            });
        }
    }
}
