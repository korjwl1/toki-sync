use axum::response::{Html, Redirect};

// ─── Dashboard redirect ─────────────────────────────────────────────────────

pub async fn dashboard_redirect() -> Redirect {
    Redirect::permanent("/dashboard")
}

// ─── Login page ─────────────────────────────────────────────────────────────

pub async fn login_page() -> Html<&'static str> {
    Html(include_str!("../../../static/login.html"))
}

// ─── Dashboard page ─────────────────────────────────────────────────────────

pub async fn dashboard_page() -> Html<&'static str> {
    Html(include_str!("../../../static/dashboard.html"))
}
