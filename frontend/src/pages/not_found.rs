use yew::prelude::*;
use yew_router::prelude::*;

use crate::components::ui::{Empty, PageHeader};
use crate::Route;

#[function_component(NotFoundPage)]
pub fn not_found() -> Html {
    html! {
        <>
            <PageHeader title="Page not found" />
            <Empty><p>{ "That page does not exist." }</p><Link<Route> to={Route::Dashboard}>{ "Back to the dashboard" }</Link<Route>></Empty>
        </>
    }
}
