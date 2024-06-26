use rocket::{
    futures::{SinkExt, StreamExt},
    State,
};
use tokio::sync::broadcast::error::RecvError;

/// Handles all the raw events being streamed from balancers and parses and filters them into only the events we care about.
pub struct EventBus {
    events_rx: tokio::sync::mpsc::Receiver<String>,

    bus_tx: tokio::sync::broadcast::Sender<EventBusEvent>,
}

impl EventBus {
    pub fn new(events_rx: tokio::sync::mpsc::Receiver<String>) -> Self {
        let (bus_tx, _) = tokio::sync::broadcast::channel(100);
        Self { events_rx, bus_tx }
    }

    #[must_use]
    pub fn spawn(mut self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run().await;
            warn!("EventBus task ended");
        })
    }

    pub async fn run(&mut self) {
        loop {
            tokio::select! {
                Some(event) = self.events_rx.recv() => {
                    self.handle_event(event);
                }
                else => {
                    break;
                }
            }
        }
    }

    fn handle_event(&self, event: String) {
        info!("Received event: {}", event);
        if self.bus_tx.receiver_count() == 0 {
            return;
        }
        match self.bus_tx.send(event) {
            Ok(count) => {
                info!("Event sent to {count} subscribers");
            }
            Err(err) => {
                error!("Error sending event to subscribers: {}", err);
            }
        }
    }

    pub fn subscriber(&self) -> EventBusSubscriber {
        EventBusSubscriber::new(self.bus_tx.clone())
    }
}

type EventBusEvent = String;

/// Enables subscriptions to the event bus
pub struct EventBusSubscriber {
    bus_tx: tokio::sync::broadcast::Sender<EventBusEvent>,
}

impl EventBusSubscriber {
    pub fn new(bus_tx: tokio::sync::broadcast::Sender<EventBusEvent>) -> Self {
        Self { bus_tx }
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<EventBusEvent> {
        self.bus_tx.subscribe()
    }
}

#[get("/state/stream")]
pub fn event_stream(
    ws: rocket_ws::WebSocket,
    event_bus: &State<EventBusSubscriber>,
) -> rocket_ws::Channel<'static> {
    let mut bus_rx = event_bus.subscribe();
    ws.channel(move |mut stream| {
        Box::pin(async move {
            loop {
                tokio::select! {
                    result = bus_rx.recv() => {
                        let event = match result {
                            Ok(event) => event,
                            Err(RecvError::Lagged(_)) => {
                                warn!("Event bus lagging");
                                continue;
                            }
                            Err(RecvError::Closed) => {
                                info!("Event bus closed");
                                break;
                            }
                        };
                        if let Err(err) = stream.send(rocket_ws::Message::text(event)).await {
                            error!("Error sending event to Event bus WebSocket: {}", err);
                            break;
                        }
                    }
                    Some(msg) = stream.next() => {
                        match msg {
                            Ok(rocket_ws::Message::Close(_)) => {
                                info!("Event bus WebSocket closed");
                                break;
                            }
                            Ok(rocket_ws::Message::Ping(ping)) => {
                                if let Err(err) = stream.send(rocket_ws::Message::Pong(ping)).await {
                                    error!("Error sending Pong to Event bus WebSocket: {}", err);
                                    break;
                                }
                            }
                            Err(rocket_ws::result::Error::Io(err)) if err.kind() == std::io::ErrorKind::BrokenPipe => {
                                warn!("Event bus WebSocket connection broken");
                                break;
                            }
                            Err(err) => {
                                error!("Error receiving message from Event bus WebSocket: {}", err);
                                break;
                            }
                            msg => {
                                warn!("Unexpected message from Event bus WebSocket: {:?}", msg);
                            }
                        }
                    }
                    else => {
                        warn!("Event bus WebSocket stream ending");
                        break;
                    }
                }
            }
            Ok(())
        })
    })
}
