use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use midir::{Ignore, MidiInput, MidiInputConnection};

use crate::message::{KeyEvent, parse_message};
use crate::ports::choose_port;

const CLIENT_NAME: &str = "Clavium";
/// Tiempo antes de reintentar un puerto que otra app tenía ocupado.
const BUSY_RETRY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq)]
pub enum MidiEvent {
    /// La lista de puertos de entrada cambió.
    PortsChanged(Vec<String>),
    Connected {
        port: String,
    },
    Disconnected {
        port: String,
    },
    /// No se pudo abrir el puerto; en Windows suele ser porque otra app lo usa.
    ConnectFailed {
        port: String,
        reason: String,
    },
    /// Una tecla o el pedal, con el instante en que llegó el mensaje.
    Key {
        event: KeyEvent,
        at: Instant,
    },
}

#[derive(Debug, Clone)]
pub struct ServiceConfig {
    /// Puerto que se prefiere si está conectado (el último que se usó).
    pub preferred_port: Option<String>,
    /// Cada cuánto se vuelve a mirar la lista de puertos.
    pub poll_interval: Duration,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        ServiceConfig {
            preferred_port: None,
            poll_interval: Duration::from_millis(1000),
        }
    }
}

/// Hilo en segundo plano que busca el teclado, se conecta y se reconecta.
/// Los eventos se leen con [`MidiService::drain`]; `wake` se llama después
/// de cada evento para que la interfaz se redibuje sin esperar al próximo cuadro.
pub struct MidiService {
    rx: Receiver<MidiEvent>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MidiService {
    pub fn start(config: ServiceConfig, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
            thread::Builder::new()
                .name("clavium-midi".into())
                .spawn(move || run(config, tx, wake, stop))
                .expect("no se pudo crear el hilo de MIDI")
        };
        MidiService {
            rx,
            stop,
            thread: Some(thread),
        }
    }

    /// Eventos recibidos desde la última llamada, sin bloquear.
    pub fn drain(&self) -> impl Iterator<Item = MidiEvent> + '_ {
        self.rx.try_iter()
    }
}

impl Drop for MidiService {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    config: ServiceConfig,
    tx: Sender<MidiEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    stop: Arc<AtomicBool>,
) {
    let send = |event: MidiEvent| {
        let _ = tx.send(event);
        wake();
    };
    let mut preferred = config.preferred_port;
    let mut current: Option<(String, MidiInputConnection<()>)> = None;
    let mut busy: Vec<(String, Instant)> = Vec::new();
    let mut last_names: Option<Vec<String>> = None;

    while !stop.load(Ordering::Relaxed) {
        if let Ok(mut input) = MidiInput::new(CLIENT_NAME) {
            let ports: Vec<_> = input
                .ports()
                .into_iter()
                .filter_map(|p| input.port_name(&p).ok().map(|name| (p, name)))
                .collect();
            let names: Vec<String> = ports.iter().map(|(_, n)| n.clone()).collect();

            if last_names.as_ref() != Some(&names) {
                send(MidiEvent::PortsChanged(names.clone()));
                last_names = Some(names.clone());
            }

            if let Some((port, _)) = &current
                && !names.contains(port)
            {
                let (port, connection) = current.take().expect("hay conexión");
                connection.close();
                send(MidiEvent::Disconnected { port });
            }

            busy.retain(|(name, since)| names.contains(name) && since.elapsed() < BUSY_RETRY);
            let skip: Vec<String> = busy.iter().map(|(n, _)| n.clone()).collect();

            if current.is_none()
                && let Some(i) = choose_port(&names, preferred.as_deref(), &skip)
            {
                let (port, name) = &ports[i];
                input.ignore(Ignore::All);
                let key_tx = tx.clone();
                let key_wake = wake.clone();
                let result = input.connect(
                    port,
                    "clavium-in",
                    move |_, bytes, _| {
                        if let Some(event) = parse_message(bytes) {
                            let at = Instant::now();
                            let _ = key_tx.send(MidiEvent::Key { event, at });
                            key_wake();
                        }
                    },
                    (),
                );
                match result {
                    Ok(connection) => {
                        preferred = Some(name.clone());
                        current = Some((name.clone(), connection));
                        send(MidiEvent::Connected { port: name.clone() });
                    }
                    Err(error) => {
                        busy.push((name.clone(), Instant::now()));
                        send(MidiEvent::ConnectFailed {
                            port: name.clone(),
                            reason: error.to_string(),
                        });
                    }
                }
            }
        }

        let deadline = Instant::now() + config.poll_interval;
        while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(50));
        }
    }

    if let Some((_, connection)) = current {
        connection.close();
    }
}
