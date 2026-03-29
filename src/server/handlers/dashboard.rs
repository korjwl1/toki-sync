use axum::response::{Html, Redirect};

// ─── Admin redirect ─────────────────────────────────────────────────────────

pub async fn admin_redirect() -> Redirect {
    Redirect::permanent("/admin")
}

// ─── Login page ─────────────────────────────────────────────────────────────

pub async fn login_page() -> Html<&'static str> {
    Html(include_str!("../../../static/login.html"))
}

// ─── Admin page ─────────────────────────────────────────────────────────────

pub async fn admin_page() -> Html<&'static str> {
    Html(include_str!("../../../static/admin.html"))
}
