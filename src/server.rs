
use std::collections::HashMap;
use std::env;
use std::net::TcpListener;
use std::sync::{Arc, mpsc, RwLock};

#[cfg(target_family="unix")]
use std::os::unix::net::UnixListener;

mod util;
mod server_util;

use crate::util::{
    parse_arg,
    ClientState, GuessProtocol as Ptcl,
    ProtocolReader, ProtocolWriter,
};
use crate::server_util::{ServerInternalMessage as IMsg};

type Sender = mpsc::Sender<IMsg<Ptcl>>;
struct ServerState {
    id_generator: usize,
    available_clients: HashMap<String, (ClientState, Sender)>,
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
    let (tx, receiver) = mpsc::channel::<(Box<dyn ProtocolReader<Ptcl> + Send>,
                                          Box<dyn ProtocolWriter<Ptcl> + Send>)>();
    #[cfg_attr(not(target_family="unix"), allow(unused_variables))]
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
                
                // Establish joint message channel
                let (tx, joint_rx) = mpsc::channel::<IMsg<Ptcl>>();
                
                // Establish connection to client
                if let Ok(mut sw) = ServerWorker::new(
                                            (&*client.0, &*client.1),
                                            password, store, tx.clone()) {
                    // Add network listener to the joint channel
                    let _ = IMsg::<Ptcl>::spawn_network_listener(client.0, tx.clone());
                    
                    // "At this moment, the server answers to any requests the client sends to the server. For unknown requests, the server must respond as well, such that client can identify it as an error."
                    while let Ok(msg) = joint_rx.recv() {
                        if sw.handle_message(msg, &*client.1).is_err() {
                            break;
                        }
                    }
                }
            });
        }
    }
}

/// Server-side representation of a client.
struct ServerWorker {
    /// Reference to the server storage
    store: Store,
    /// Client id
    id: String,
    /// Client game state
    client_state: ClientState,
    /// Transmitter for the joint channel of the client
    tx: Sender,
    /// Transmitter for the joint channel of the opponent
    tx_to_opponent: Option<Sender>,
    /// Opponent word
    opponent_word: Option<String>,
}

impl ServerWorker {
    fn new<R,W>(
        (client_r, client_w): (&R, &W),
        password: String,
        store: Store,
        tx: Sender,
    ) -> Result<Self, ()>
        where R: ProtocolReader<Ptcl> + ?Sized,
              W: ProtocolWriter<Ptcl> + ?Sized
    {
        // "Upon connection - the server must send a message to the client - initiating the communication. Client upon receiving it - answers to the server with a password."
        let _ = client_w.write(&Ptcl::ServerHello);
        match client_r.read() {
            Ok(Ptcl::ClientPassword(p)) if p == password => {},
            _ => return Err(()),
        }
        
        // "This initial exchange then ends with server either disconnecting the client (wrong password) or assigning the client an ID and sending the ID back to the client."
        let id = {
            let mut lock = store.write().unwrap();
            let id = lock.id_generator;
            lock.id_generator += 1;
            let id = id.to_string();
            lock.available_clients.insert(id.clone(), (ClientState::Free, tx.clone()));
            id
        };
        let _ = client_w.write(&Ptcl::ConnectionEstablished(id.clone()));
        
        Ok(Self{store, id, tx, client_state: ClientState::Free,
                tx_to_opponent: None, opponent_word: None})
    }
    
    fn handle_message<W>(
        &mut self,
        msg: IMsg<Ptcl>,
        client_w: &W,
    ) -> Result<(),()>
        where W: ProtocolWriter<Ptcl> + ?Sized
    {
        match msg {
            // Client was likely disconnected, therefore terminate
            IMsg::Error => {
                if let Some(tx) = &self.tx_to_opponent {
                    let _ = tx.send(IMsg::OpponentDisconnected);
                }
                return Err(());
            },
            IMsg::OpponentDisconnected => {
                let _ = client_w.write(&Ptcl::ServerOpponentDisconnected);
                self.reset_state();
            }
            // Client got connected as a guesser
            IMsg::OpponentAssigned(tx, id, w) if self.client_state == ClientState::Free => {
                self.client_state = ClientState::Guesser;
                self.tx_to_opponent = Some(tx);
                self.opponent_word = Some(w);
                let _ = client_w.write(&Ptcl::OpponentConnectionEstablished(
                                                id.clone(), ClientState::Guesser));
            },
            IMsg::WordFound => {
                let _ = client_w.write(&Ptcl::ServerWordFound);
                self.reset_state();
            },
            // Resend messages from opponent to client
            IMsg::Opponent(msg) => {
                let _ = client_w.write(&msg);
            },
            // Invalid message was read, inform client
            IMsg::Network(Err(_)) => {
                let _ = client_w.write(&Ptcl::UnrecognizedMessageError);
            },
            // Process valid messages from client
            IMsg::Network(Ok(msg)) => match (&self.client_state, msg) {
                (_, Ptcl::ClientListOpponents) => {
                    let lock = self.store.read().unwrap();
                    let _ = client_w.write(&Ptcl::ServerOpponentList(
                        lock.available_clients.iter()
                            .filter_map(|(k,v)|
                                if *k != self.id && v.0 == ClientState::Free {
                                    Some(k.to_string())
                                } else {None}).collect::<Vec<String>>()));
                },
                (_, Ptcl::ClientSelectOpponent(oid, word)) => {
                    let mut lock = self.store.write().unwrap();
                    
                    match lock.available_clients.get_mut(&oid)
                                        .filter(|e| e.0 == ClientState::Free) {
                        Some(ol) if oid != self.id => {
                            ol.0 = ClientState::Guesser;
                            let _ = ol.1.send(IMsg::OpponentAssigned(
                                                        self.tx.clone(),
                                                        self.id.clone(),
                                                        word.clone()));
                            self.tx_to_opponent = Some(ol.1.clone());
                            self.client_state = ClientState::Riddlemaker(word);
                            if let Some(me) = lock.available_clients.get_mut(&self.id) {
                                me.0 = self.client_state.clone();
                            };
                            let _ = client_w.write(&Ptcl::OpponentConnectionEstablished(
                                                                oid.clone(),
                                                                self.client_state.clone()));
                        },
                        _ => {
                            let _ = client_w.write(
                                &Ptcl::OpponentConnectionNotEstablished(oid));
                        }
                    }
                },
                (ClientState::Guesser, Ptcl::ClientGuess(word)) => {
                    if let Some(tx) = &self.tx_to_opponent {
                        match &self.opponent_word {
                            Some(w) if *w == word => {
                                let _ = tx.send(IMsg::WordFound);
                                let _ = client_w.write(&Ptcl::ServerWordFound);
                                self.reset_state();
                            },
                            _ => {
                                let _ = tx.send(IMsg::Opponent(Ptcl::ClientGuess(word)));
                            },
                        }
                    }
                }
                (ClientState::Riddlemaker(_), Ptcl::ClientHint(word)) => {
                    if let Some(tx) = &self.tx_to_opponent {
                        let _ = tx.send(IMsg::Opponent(Ptcl::ClientHint(word)));
                    }
                },
                // Logic error: message was valid, but not currently expected
                //   e.g.: Riddlemaker sending a Guess message, or player requesting a new connection while in a game, etc.
                _ => {
                    let _ = client_w.write(&Ptcl::LogicError);
                },
            },
            _ => {},
        }
        Ok(())
    }
    /// State reset, such as when a game was won or forfeited
    fn reset_state(&mut self) {
        self.client_state = ClientState::Free;
        self.tx_to_opponent = None;
        self.opponent_word = None;
        let mut lock = self.store.write().unwrap();
        if let Some(e) = lock.available_clients.get_mut(&self.id) {
            e.0 = ClientState::Free;
        }
    }
}

impl Drop for ServerWorker {
    // Connection lost: clean up
    fn drop(&mut self) {
        let mut lock = self.store.write().unwrap();
        let _ = lock.available_clients.remove(&self.id);
    }
}
