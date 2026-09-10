/// Regression test: same auction input must produce byte-identical JSON output
/// across N independent solve calls.
///
/// This guards against non-determinism from:
///   - Rayon parallel iteration order in strategies 1-6
///   - HashMap iteration order in price maps
///   - Async aggregator results arriving in different orders (strategy 7)
///   - Tie-breaking in solution sorting
use solver_engine::models::auction::AuctionInstance;
use solver_engine::models::liquidity::{
    ConstantProductPool, Liquidity, LiquidityTokenBalance, LiquidityTokenMap,
};
use solver_engine::models::order::{Order, OrderClass, OrderKind};

fn make_order(uid: &str, sell: &str, buy: &str, sell_amt: u128, buy_amt: u128) -> Order {
    Order {
        uid: uid.to_string(),
        sell_token: sell.to_string(),
        buy_token: buy.to_string(),
        sell_amount: sell_amt.to_string(),
        buy_amount: buy_amt.to_string(),
        fee_amount: "0".to_string(),
        kind: OrderKind::Sell,
        partially_fillable: false,
        class: OrderClass::Market,
        sell_token_balance: None,
        buy_token_balance: None,
        signing_scheme: None,
        signature: None,
        receiver: None,
        app_data: None,
        valid_to: None,
    }
}

fn make_cp(id: &str, t0: &str, r0: u128, t1: &str, r1: u128) -> Liquidity {
    let mut tokens = LiquidityTokenMap::new();
    tokens.insert(
        t0.to_string(),
        LiquidityTokenBalance { balance: r0.to_string() },
    );
    tokens.insert(
        t1.to_string(),
        LiquidityTokenBalance { balance: r1.to_string() },
    );
    Liquidity::ConstantProduct(ConstantProductPool {
        id: id.to_string(),
        address: format!("0x{id}"),
        tokens,
        fee: "0.003".to_string(),
        router: None,
        gas_estimate: String::new(),
    })
}

fn make_auction(orders: Vec<Order>, liquidity: Vec<Liquidity>) -> AuctionInstance {
    AuctionInstance {
        id: 99,
        tokens: Default::default(),
        orders,
        liquidity,
        effective_gas_price: "1".to_string(),
        deadline: None,
        chain_id: None,
        block: None,
    }
}

/// Run the solver 10 times on the same auction and assert byte-identical JSON output.
#[tokio::test]
async fn same_auction_produces_identical_output_10_times() {
    let auction = make_auction(
        vec![
            make_order("alice", "0xweth", "0xusdc", 1_000, 900),
            make_order("bob", "0xusdc", "0xweth", 1_000, 900),
            make_order("charlie", "0xa", "0xb", 1_000_000_000, 1),
        ],
        vec![
            make_cp("p1", "0xweth", 1_000_000_000_000u128, "0xusdc", 2_000_000_000_000u128),
            make_cp("p2", "0xa", 500_000_000_000u128, "0xb", 1_000_000_000_000u128),
        ],
    );

    // Collect 10 serialized outputs
    let mut outputs: Vec<String> = Vec::with_capacity(10);
    for _ in 0..10 {
        let outcome = solver_engine::solver::solve(auction.clone()).await;
        let json = serde_json::to_string(&outcome.response)
            .expect("response must be serializable");
        outputs.push(json);
    }

    // All outputs must be identical
    let first = &outputs[0];
    for (i, output) in outputs.iter().enumerate().skip(1) {
        assert_eq!(
            first, output,
            "Run {} produced different output from run 0.\n\nRun 0: {}\nRun {}: {}",
            i, first, i, output
        );
    }
}

/// Verify that solutions are sorted score-descending with stable tie-breaking.
#[tokio::test]
async fn solutions_sorted_by_score_descending() {
    let auction = make_auction(
        vec![
            make_order("alice", "0xweth", "0xusdc", 1_000, 900),
            make_order("bob", "0xusdc", "0xweth", 1_000, 900),
        ],
        vec![make_cp(
            "p1",
            "0xweth",
            1_000_000_000_000u128,
            "0xusdc",
            2_000_000_000_000u128,
        )],
    );

    let outcome = solver_engine::solver::solve(auction).await;
    let solutions = &outcome.response.solutions;

    // Scores must be non-increasing
    for window in solutions.windows(2) {
        let score_a = extract_score_u128(&window[0]);
        let score_b = extract_score_u128(&window[1]);
        assert!(
            score_a >= score_b,
            "Solutions not sorted: score[{}] = {} < score[{}] = {}",
            window[0].id, score_a, window[1].id, score_b
        );
    }

    // IDs must be sequential starting at 0
    for (i, sol) in solutions.iter().enumerate() {
        assert_eq!(sol.id, i as u64, "Solution ID must equal its index");
    }
}

fn extract_score_u128(sol: &solver_engine::models::solution::Solution) -> u128 {
    match &sol.score {
        Some(solver_engine::models::solution::Score::Solver { score }) => {
            score.parse().unwrap_or(0)
        }
        Some(solver_engine::models::solution::Score::RiskAdjusted { success_probability }) => {
            (*success_probability * 1_000_000.0) as u128
        }
        None => 0,
    }
}
