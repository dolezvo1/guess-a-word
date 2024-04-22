
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
        (client_r, client_w): (&mut R, &mut W),
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
        client_w: &mut W,
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


#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc, RwLock};
    use std::collections::{HashMap, VecDeque};
    use crate::ServerState;
    
    fn empty_store() -> Store {
        Arc::new(RwLock::new(
            ServerState {
                id_generator: 7,
                available_clients: HashMap::new(),
        }))
    }
    
    fn client_rw() -> (Box<VecDeque<u8>>, Box<VecDeque<u8>>) {
        (Box::new(VecDeque::<u8>::new()), Box::new(VecDeque::<u8>::new()))
    }
    
    #[test]
    fn test_can_establish_communication() {
        // Prepare
        let store = empty_store();
        let (mut client_r, mut client_w) = client_rw();
        let (tx, rx) = mpsc::channel::<IMsg<Ptcl>>();
        let _ = client_r.as_mut().write(&Ptcl::ClientPassword("password".to_string()));
        
        // Execute
        let worker = ServerWorker::new((client_r.as_mut(), client_w.as_mut()),
                                           "password".to_string(),
                                           store.clone(), tx).unwrap();
        
        // Empty communication queues
        let hello: Ptcl = client_w.as_mut().read().unwrap();
        let connection_established: Ptcl = client_w.as_mut().read().unwrap();
        
        // Test
        assert!(client_r.is_empty());
        assert!(client_w.is_empty());
        assert!(rx.try_recv().is_err());
        assert_eq!(hello, Ptcl::ServerHello);
        assert_eq!(connection_established, Ptcl::ConnectionEstablished("7".to_string()));
        
        assert_eq!(worker.id, "7".to_string());
        assert_eq!(worker.client_state, ClientState::Free);
        assert_eq!(worker.opponent_word, None);
        
        let lock = store.read().unwrap();
        assert_eq!(lock.id_generator, 8);
        assert_eq!(lock.available_clients.len(), 1);
        assert_eq!(lock.available_clients["7"].0, ClientState::Free);
    }
    
    #[test]
    fn test_rejects_invalid_password() {
        // Prepare
        let store = empty_store();
        let (mut client_r, mut client_w) = client_rw();
        let (tx, rx) = mpsc::channel::<IMsg<Ptcl>>();
        let _ = client_r.as_mut().write(&Ptcl::ClientPassword("not_password".to_string()));
        
        // Execute
        let worker = ServerWorker::new((client_r.as_mut(), client_w.as_mut()),
                                           "password".to_string(),
                                           store.clone(), tx);
        
        // Empty communication queues
        let hello: Ptcl = client_w.as_mut().read().unwrap();
        
        // Test
        assert!(client_r.is_empty());
        assert!(client_w.is_empty());
        assert!(rx.try_recv().is_err());
        assert_eq!(hello, Ptcl::ServerHello);
        assert!(worker.is_err());
        
        let lock = store.read().unwrap();
        assert_eq!(lock.id_generator, 7);
        assert_eq!(lock.available_clients.len(), 0);
    }
    
    // TODO: test can handle user disconnection
    
    #[test]
    fn test_rejects_connection_nonexistent() {
        // Prepare
        let store = empty_store();
        let (mut client_r, mut client_w) = client_rw();
        let (tx, rx) = mpsc::channel::<IMsg<Ptcl>>();
        let _ = client_r.as_mut().write(&Ptcl::ClientPassword("password".to_string()));
        let mut worker = ServerWorker::new((client_r.as_mut(), client_w.as_mut()),
                                           "password".to_string(),
                                           store.clone(), tx).unwrap();
        
        // Execute
        let r = worker.handle_message(IMsg::Network(Ok(
                                        Ptcl::ClientSelectOpponent("404".to_string(),
                                                                   "missing".to_string()))),
                              client_w.as_mut());
        
        // Empty communication queues
        let hello: Ptcl = client_w.as_mut().read().unwrap();
        let connection_established: Ptcl = client_w.as_mut().read().unwrap();
        let opponent_not_established: Ptcl = client_w.as_mut().read().unwrap();
        
        // Test
        assert_eq!(r, Ok(()));
        assert!(client_r.is_empty());
        assert!(client_w.is_empty());
        assert!(rx.try_recv().is_err());
        assert_eq!(hello, Ptcl::ServerHello);
        assert_eq!(connection_established,
                   Ptcl::ConnectionEstablished("7".to_string()));
        assert_eq!(opponent_not_established,
                   Ptcl::OpponentConnectionNotEstablished("404".to_string()));
        
        assert_eq!(worker.id, "7".to_string());
        assert_eq!(worker.client_state, ClientState::Free);
        assert_eq!(worker.opponent_word, None);
        
        let lock = store.read().unwrap();
        assert_eq!(lock.id_generator, 8);
        assert_eq!(lock.available_clients.len(), 1);
        assert_eq!(lock.available_clients["7"].0, ClientState::Free);
    }
    
    #[test]
    fn test_rejects_connection_self() {
        // Prepare
        let store = empty_store();
        let (mut client_r, mut client_w) = client_rw();
        let (tx, rx) = mpsc::channel::<IMsg<Ptcl>>();
        let _ = client_r.as_mut().write(&Ptcl::ClientPassword("password".to_string()));
        let mut worker = ServerWorker::new((client_r.as_mut(), client_w.as_mut()),
                                           "password".to_string(),
                                           store.clone(), tx).unwrap();
        
        // Execute
        let r = worker.handle_message(IMsg::Network(Ok(
                                        Ptcl::ClientSelectOpponent("7".to_string(),
                                                                   "self".to_string()))),
                              client_w.as_mut());
        
        // Empty communication queues
        let hello: Ptcl = client_w.as_mut().read().unwrap();
        let connection_established: Ptcl = client_w.as_mut().read().unwrap();
        let opponent_not_established: Ptcl = client_w.as_mut().read().unwrap();
        
        // Test
        assert_eq!(r, Ok(()));
        assert!(client_r.is_empty());
        assert!(client_w.is_empty());
        assert!(rx.try_recv().is_err());
        assert_eq!(hello, Ptcl::ServerHello);
        assert_eq!(connection_established,
                   Ptcl::ConnectionEstablished("7".to_string()));
        assert_eq!(opponent_not_established,
                   Ptcl::OpponentConnectionNotEstablished("7".to_string()));
        
        assert_eq!(worker.id, "7".to_string());
        assert_eq!(worker.client_state, ClientState::Free);
        assert_eq!(worker.opponent_word, None);
        
        let lock = store.read().unwrap();
        assert_eq!(lock.id_generator, 8);
        assert_eq!(lock.available_clients.len(), 1);
        assert_eq!(lock.available_clients["7"].0, ClientState::Free);
    }
    
    #[test]
    fn test_allows_game_start() {
        // Prepare
        let store = empty_store();
        let (mut client1_r, mut client1_w) = client_rw();
        let (mut client2_r, mut client2_w) = client_rw();
        let (tx1, rx1) = mpsc::channel::<IMsg<Ptcl>>();
        let (tx2, rx2) = mpsc::channel::<IMsg<Ptcl>>();
        let _ = client1_r.as_mut().write(&Ptcl::ClientPassword("password".to_string()));
        let _ = client2_r.as_mut().write(&Ptcl::ClientPassword("password".to_string()));
        let mut worker1 = ServerWorker::new((client1_r.as_mut(), client1_w.as_mut()),
                                           "password".to_string(),
                                           store.clone(), tx1).unwrap();
        let mut worker2 = ServerWorker::new((client2_r.as_mut(), client2_w.as_mut()),
                                           "password".to_string(),
                                           store.clone(), tx2).unwrap();
        
        // Execute
        let r1 = worker1.handle_message(IMsg::Network(Ok(
                                        Ptcl::ClientSelectOpponent("8".to_string(),
                                                                   "valid".to_string()))),
                              client1_w.as_mut());
        let r2 = worker2.handle_message(rx2.recv().unwrap(), client2_w.as_mut());
        
        // Empty communication queues
        let hello1: Ptcl = client1_w.as_mut().read().unwrap();
        let connection_established1: Ptcl = client1_w.as_mut().read().unwrap();
        let opponent_established1: Ptcl = client1_w.as_mut().read().unwrap();
        let hello2: Ptcl = client2_w.as_mut().read().unwrap();
        let connection_established2: Ptcl = client2_w.as_mut().read().unwrap();
        let opponent_established2: Ptcl = client2_w.as_mut().read().unwrap();
        
        // Test
        let state1 = ClientState::Riddlemaker("valid".to_string());
        let state2 = ClientState::Guesser;
        assert_eq!(r1, Ok(()));
        assert_eq!(r2, Ok(()));
        assert!(client1_r.is_empty());
        assert!(client1_w.is_empty());
        assert!(client2_r.is_empty());
        assert!(client2_w.is_empty());
        assert!(rx1.try_recv().is_err());
        assert!(rx2.try_recv().is_err());
        assert_eq!(hello1, Ptcl::ServerHello);
        assert_eq!(connection_established1,
                   Ptcl::ConnectionEstablished("7".to_string()));
        assert_eq!(opponent_established1,
                   Ptcl::OpponentConnectionEstablished("8".to_string(), state1.clone()));
        assert_eq!(hello2, Ptcl::ServerHello);
        assert_eq!(connection_established2,
                   Ptcl::ConnectionEstablished("8".to_string()));
        assert_eq!(opponent_established2,
                   Ptcl::OpponentConnectionEstablished("7".to_string(), state2.clone()));
        
        assert_eq!(worker1.id, "7".to_string());
        assert_eq!(worker1.client_state, state1);
        assert_eq!(worker1.opponent_word, None);
        assert_eq!(worker2.id, "8".to_string());
        assert_eq!(worker2.client_state, state2);
        assert_eq!(worker2.opponent_word, Some("valid".to_string()));
        
        let lock = store.read().unwrap();
        assert_eq!(lock.id_generator, 9);
        assert_eq!(lock.available_clients.len(), 2);
        assert_eq!(lock.available_clients["7"].0, state1);
        assert_eq!(lock.available_clients["8"].0, state2);
    }
    
    // TODO: test can handle connection attempt busy
    #[test]
    fn test_rejects_connection_busy() {
        
        assert!(false);
    }
    
    // TODO: test can handle user disconnection during game
    // TODO: test can handle guess attempt
    // TODO: test can handle hints
    // TODO: test can handle word found
    // TODO: test can handle connection after a game ended
    
}
