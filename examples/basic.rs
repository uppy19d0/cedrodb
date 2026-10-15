use cedrodb::Database;
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "cedro-data".into());
    let database = Database::open(directory)?;
    let mut transaction = database.begin_write()?;
    transaction.put(
        "orders",
        "order:001",
        json!({"customer": "Ana", "amount": 125.5}),
    )?;
    transaction.put(
        "orders",
        "order:002",
        json!({"customer": "Luis", "amount": 74.5}),
    )?;
    transaction.commit()?;
    println!("Order: {:?}", database.get("orders", "order:001")?);
    println!("Amounts: {:?}", database.aggregate("orders", "amount")?);
    Ok(())
}
