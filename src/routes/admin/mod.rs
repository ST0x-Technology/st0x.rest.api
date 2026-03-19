mod logs;
mod registry;

use rocket::Route;

pub fn routes() -> Vec<Route> {
    let mut routes = Vec::new();
    routes.extend(registry::routes());
    routes.extend(logs::routes());
    routes
}
