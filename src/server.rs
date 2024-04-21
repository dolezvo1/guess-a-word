
use std::collections::HashMap;
use std::env;
use std::net::TcpListener;
use std::sync::{Arc, mpsc, mpsc::Sender, RwLock};

#[cfg(target_family="unix")]
use std::os::unix::net::UnixListener;

mod util;
use crate::util::{
    parse_arg,
    ClientState, InternalMessage as IMsg, GuessProtocol as Ptcl,
    ProtocolReader, ProtocolWriter,
    spawn_network_listener,
};

/*
fn main() {
    // The object that we will serialize.
    let target: Option<String>  = Some("hello world".to_string());

    //let encoded: Vec<u8> = bincode::serialize(&target).unwrap();
    let decoded: Option<String> = bincode::deserialize(&encoded[..]).unwrap();
    assert_eq!(target, decoded);
    println!("{:?}", encoded);
}
*/

struct ServerState {
    id_generator: usize,
    available_clients: HashMap<String, (ClientState, Sender<IMsg<Ptcl>>)>,
}
type Store = Arc<RwLock<ServerState>>;

static ACCEPTED_OPTIONS: [&str; 3] = [
    "--server-password",
    "--tcp-port",
    "--unix-socket-path",
];

#[tokio::main]
async fn main() -> std::io::Result<()> {
    
    let args: Vec<_> = env::args().collect();
    let (tx, receiver) = mpsc::channel::<(Box<dyn ProtocolReader<Ptcl> + Send + Unpin>,
                                          Box<dyn ProtocolWriter<Ptcl> + Send>)>();
    let (tx_tcp, tx_unix_socket) = (tx.clone(), tx);
    
    let password = parse_arg::<String>("--server-password", &args, &ACCEPTED_OPTIONS)
                        .expect("Server password must be provided (`--server-password`)");
    let store = Arc::new(RwLock::new(
        ServerState {
            id_generator: 1,
            available_clients: HashMap::new(),
        }));
    
    // Create TCP listener thread
    let tcp_port = parse_arg("--tcp-port", &args, &ACCEPTED_OPTIONS).unwrap_or(7777);
    tokio::spawn(async move {
        let tcp_listener = match TcpListener::bind(("127.0.0.1", tcp_port)) {
            Ok(tcp_listener) => tcp_listener,
            _ => { return; }
        };
        
        for stream in tcp_listener.incoming() {
            let _ = stream.map(|stream| stream.try_clone().map(|clone| {
                let _ = tx_tcp.send((Box::new(stream), Box::new(clone)));
            }));
        }
        unreachable!();
    });
    
    // Create Unix socket listener
    #[cfg(target_family="unix")] {
        let unix_socket_path = parse_arg("--unix-socket-path", &args, &ACCEPTED_OPTIONS)
                                    .unwrap_or_else(|| "guessaword".to_string());
        tokio::spawn(async move {
            // Clean up the socket if it already exists
            if std::fs::metadata(unix_socket_path).is_ok() {
                std::fs::remove_file(unix_socket_path)?;
            }

            // Create a Unix listener on the socket path
            let listener = UnixListener::bind(unix_socket_path)?;
            println!("Server is running on {}", unix_socket_path);
            
            // Loop over incoming connections
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        tx_unix_socket.send(2);
                    }
                    Err(e) => {
                        eprintln!("Failed to connect: {}", e);
                    }
                }
            }
            unreachable!();
        });
    }
    
    // Start a task for each connection received through channel
    loop {
        if let Ok(client) = receiver.recv() {
            let (password, store) = (password.clone(), store.clone());
            tokio::spawn(async move {
                handle_new_client(client, password, store);
            });
        }
    }
}

fn handle_new_client<R,W>(
    (client_r, client_w): (Box<R>, Box<W>),
    password: String,
    store: Store
)
    where R: ProtocolReader<Ptcl> + Send + ?Sized + 'static,
          W: ProtocolWriter<Ptcl> + Send + ?Sized
{
    // "Upon connection - the server must send a message to the client - initiating the communication. Client upon receiving it - answers to the server with a password."
    let _ = client_w.write(&Ptcl::ServerHello);
    match client_r.read() {
        Ok(Ptcl::ClientPassword(p)) if p == password => {},
        _ => return,
    }
    
    // "This initial exchange then ends with server either disconnecting the client (wrong password) or assigning the client an ID and sending the ID back to the client."
    let (tx, joint_rx) = mpsc::channel::<IMsg<Ptcl>>();
    let assigned_id = {
        let mut lock = store.write().unwrap();
        let new_id = lock.id_generator;
        lock.id_generator += 1;
        let new_id = new_id.to_string();
        lock.available_clients.insert(new_id.clone(), (ClientState::Free, tx.clone()));
        new_id
    };
    let _ = client_w.write(&Ptcl::ConnectionEstablished(assigned_id.clone()));
    
    // Add network listener to the joint channel
    let _ = spawn_network_listener(client_r, tx.clone());
    
    // "At this moment, the server answers to any requests the client sends to the server. For unknown requests, the server must respond as well, such that client can identify it as an error."
    let mut client_state = ClientState::Free;
    let mut tx_to_opponent: Option<Sender<IMsg<Ptcl>>> = None;
    let mut opponent_word = None;
    
    macro_rules! clean_client_state { () => {
        client_state = ClientState::Free;
        tx_to_opponent = None;
        opponent_word = None;
        let mut lock = store.write().unwrap();
        lock.available_clients.get_mut(&assigned_id).map(|e| e.0 = ClientState::Free);
    }}
    
    while let Ok(msg) = joint_rx.recv() {
        match msg {
            // Client was likely disconnected, therefore terminate
            IMsg::Error => {
                if let Some(tx) = tx_to_opponent {
                    let _ = tx.send(IMsg::OpponentDisconnected);
                }
                break
            },
            IMsg::OpponentDisconnected => {
                let _ = client_w.write(&Ptcl::ServerOpponentDisconnected);
                clean_client_state!();
            }
            // Client got connected as a guesser
            IMsg::OpponentAssigned(tx, id, w) if client_state == ClientState::Free => {
                client_state = ClientState::Guesser;
                tx_to_opponent = Some(tx);
                opponent_word = Some(w);
                let _ = client_w.write(&Ptcl::OpponentConnectionEstablished(
                                                id.clone(), ClientState::Guesser));
            },
            IMsg::WordFound => {
                let _ = client_w.write(&Ptcl::ServerWordFound);
                clean_client_state!();
            },
            // Resend messages by channel to client
            IMsg::Internal(msg) => {
                let _ = client_w.write(&msg);
            },
            // Process messages from client
            IMsg::Network(msg) => match (&client_state, msg) {
                (_, Ptcl::ClientListOpponents) => {
                    let lock = store.read().unwrap();
                    let _ = client_w.write(&Ptcl::ServerOpponentList(
                        lock.available_clients.iter()
                            .filter_map(|(k,v)|
                                if *k != assigned_id && v.0 == ClientState::Free {
                                    Some(k.to_string())
                                } else {None}).collect::<Vec<String>>()));
                },
                (_, Ptcl::ClientSelectOpponent(oid, word)) => {
                    let mut lock = store.write().unwrap();
                    
                    match lock.available_clients.get_mut(&oid)
                                        .filter(|e| e.0 == ClientState::Free) {
                        Some(ol) if oid != assigned_id => {
                            ol.0 = ClientState::Guesser;
                            let _ = ol.1.send(IMsg::OpponentAssigned(tx.clone(),
                                                        assigned_id.clone(), word.clone()));
                            tx_to_opponent = Some(ol.1.clone());
                            client_state = ClientState::Riddlemaker(word);
                            if let Some(me) = lock.available_clients.get_mut(&assigned_id) {
                                me.0 = client_state.clone();
                            };
                            let _ = client_w.write(&Ptcl::OpponentConnectionEstablished(
                                                                oid.clone(),
                                                                client_state.clone()));
                        },
                        _ => {
                            let _ = client_w.write(
                                &Ptcl::OpponentConnectionNotEstablished(oid));
                        }
                    }
                },
                
                (ClientState::Guesser, Ptcl::ClientGuess(word)) => {
                    if let Some(tx) = &tx_to_opponent {
                        match &opponent_word {
                            Some(w) if *w == word => {
                                let _ = tx.send(IMsg::WordFound);
                                let _ = client_w.write(&Ptcl::ServerWordFound);
                                clean_client_state!();
                            },
                            _ => {
                                let _ = tx.send(IMsg::Internal(Ptcl::ClientGuess(word)));
                            },
                        }
                    }
                }
                (ClientState::Riddlemaker(_), Ptcl::ClientHint(word)) => {
                    if let Some(tx) = &tx_to_opponent {
                        let _ = tx.send(IMsg::Internal(Ptcl::ClientHint(word)));
                    }
                },
                (_, Ptcl::ClientGuess(_) | Ptcl::ClientHint(_)) => {
                    // Logic error: Messages were sent by the wrong client
                    let _ = client_w.write(&Ptcl::LogicError);
                },
                
                _ => {
                    let _ = client_w.write(&Ptcl::UnrecognizedMessageError);
                },
            },
            _ => {},
        }
    }

    // Connection lost: clean up
    {
        let mut lock = store.write().unwrap();
        let _ = lock.available_clients.remove(&assigned_id);
    }
}
