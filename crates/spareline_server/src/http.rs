//! The HTTP service: the guard, every route, the event stream and the long polls.

mod api_path;
mod auth_routes;
mod event_stream;
mod guard;
pub(crate) mod js_text;
mod json_body;
mod key_routes;
mod message_routes;
mod monitor_routes;
mod number_routes;
mod phone_routes;
mod phone_view;
mod query_params;
mod revocation;
mod router;
mod screen_routes;
mod sim_routes;
mod static_routes;
mod status_routes;

pub use router::router;
