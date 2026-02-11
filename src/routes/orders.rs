use crate::error::{ApiError, ApiErrorResponse};
use crate::types::common::{TokenRef, ValidatedAddress, ValidatedFixedBytes};
use crate::types::orders::{
    OrderSummary, OrdersByTxResponse, OrdersListResponse, OrdersPagination, OrdersPaginationParams,
};
use rain_orderbook_common::raindex_client::orders::{GetOrdersFilters, RaindexOrder};
use rain_orderbook_js_api::registry::DotrainRegistry;
use rocket::serde::json::Json;
use rocket::{Route, State};

const SUBGRAPH_PAGE_SIZE: usize = 100;

#[utoipa::path(
    get,
    path = "/v1/orders/tx/{tx_hash}",
    tag = "Orders",
    params(
        ("tx_hash" = String, Path, description = "Transaction hash"),
    ),
    responses(
        (status = 200, description = "Orders from transaction", body = OrdersByTxResponse),
        (status = 202, description = "Transaction not yet indexed", body = ApiErrorResponse),
        (status = 404, description = "Transaction not found", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/tx/<tx_hash>")]
pub async fn get_orders_by_tx(
    tx_hash: ValidatedFixedBytes,
) -> Result<Json<OrdersByTxResponse>, ApiError> {
    let _ = tx_hash;
    todo!()
}

fn map_order_to_summary(order: &RaindexOrder) -> Option<OrderSummary> {
    let inputs = order.inputs_list().items();
    let outputs = order.outputs_list().items();
    let input_vault = inputs.first()?;
    let output_vault = outputs.first()?;
    let input_token = input_vault.token();
    let output_token = output_vault.token();

    Some(OrderSummary {
        order_hash: order.order_hash(),
        owner: order.owner(),
        input_token: TokenRef {
            address: input_token.address(),
            symbol: input_token.symbol().unwrap_or_default(),
            decimals: input_token.decimals(),
        },
        output_token: TokenRef {
            address: output_token.address(),
            symbol: output_token.symbol().unwrap_or_default(),
            decimals: output_token.decimals(),
        },
        output_vault_balance: output_vault.formatted_balance(),
        io_ratio: String::new(),
        created_at: order.timestamp_added().to::<u64>(),
        orderbook_id: order.orderbook(),
    })
}

#[utoipa::path(
    get,
    path = "/v1/orders/{address}",
    tag = "Orders",
    params(
        ("address" = String, Path, description = "Owner address"),
        OrdersPaginationParams,
    ),
    responses(
        (status = 200, description = "Paginated list of orders", body = OrdersListResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/<address>?<params..>", rank = 2)]
pub async fn get_orders_by_address(
    address: ValidatedAddress,
    params: OrdersPaginationParams,
    registry: &State<DotrainRegistry>,
) -> Result<Json<OrdersListResponse>, ApiError> {
    let owner = address.0;
    let page = params.page.unwrap_or(1);
    let page_size = params.page_size.unwrap_or(20);

    let registry = registry.inner().clone();
    let (orders, has_more) = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let local = tokio::task::LocalSet::new();
        rt.block_on(local.run_until(async {
            let client = registry.get_raindex_client().map_err(|e| e.to_string())?;

            let filters = GetOrdersFilters {
                owners: vec![owner],
                active: Some(true),
                ..Default::default()
            };

            let orders = client
                .get_orders(None, Some(filters), Some(page as u16))
                .await
                .map_err(|e| e.to_string())?;

            let has_more = orders.len() >= SUBGRAPH_PAGE_SIZE;
            let summaries: Vec<OrderSummary> = orders
                .iter()
                .filter_map(map_order_to_summary)
                .take(page_size as usize)
                .collect();
            Ok::<_, String>((summaries, has_more))
        }))
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .map_err(ApiError::Internal)?;

    let pagination = OrdersPagination {
        page,
        page_size,
        total_orders: 0,
        total_pages: 0,
        has_more,
    };

    Ok(Json(OrdersListResponse { orders, pagination }))
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_orders_by_tx, get_orders_by_address]
}

#[cfg(test)]
mod tests {}
