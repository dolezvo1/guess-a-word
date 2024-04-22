
use crate::protocol::{ClientState, GuessProtocol as Ptcl, ProtocolReader, ProtocolWriter};
use crate::server_util::{ServerInternalMessage as IMsg};
use crate::{Sender, Store};

/// Server-side representation of a client.
pub struct ServerWorker {
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
    pub fn new<R,W>(
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
    
    pub fn handle_message<W>(
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

