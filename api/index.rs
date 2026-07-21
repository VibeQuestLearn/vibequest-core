use tower::ServiceBuilder;
use vercel_runtime::{Error, axum::VercelLayer};
use vibequest_core::{app_state, build_router, initialize_platform};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let state = app_state();
    if let Err(error) = initialize_platform(&state).await {
        tracing::warn!(error = %error, "v3 index initialization failed");
    }

    let app = ServiceBuilder::new()
        .layer(VercelLayer::new())
        .service(build_router(state));

    vercel_runtime::run(app).await
}
