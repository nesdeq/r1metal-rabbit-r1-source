// What a stay in a network carries: connections and UDP questions raced over its addresses, DNS, TLS, HTTP, WebSocket, and the connections kept for the stay's next request.

pub mod dial;
pub mod names;
pub mod pool;
#[cfg(test)]
pub(crate) mod served;
pub mod tls;
pub mod web;
pub mod websocket;
