---
name: rust-developers
description: Rust Developer Skill
---

# Rust Developer Skill

Guidelines for writing, reviewing, or modifying Rust code in this project.

---

## 1. Test-Driven Development (TDD)

Red-Green-Refactor: failing test → minimum code to pass → clean up with tests green.

- No production code without a test. Bug fix → test reproducing it first. Feature → test describing behavior first.
- One behavior per test. Name: `test_<action>_<condition>_<expected_result>`.
- Prefer `assert_eq!` / `assert_matches!` over bare `assert!`.

### Test Organization

- Unit tests with mocks → `src/test/handlers/` (`mockall`).
- Integration tests with real infra → `src/test/integrations/` (`testcontainers`, real PostgreSQL).
- Don't mix the two.

### Mocking with `mockall`

- `Mock<Service>::new()`; set expectations with `.expect_<method>().returning(...)`.
- `.times(1)` / `.times(..)` when counts matter.
- Handler tests: `setup_rocket_mocked()` in `src/test/helpers/mocks.rs`.
- Stateful mocks: shared `Arc<RwLock<HashMap<_,_>>>` cloned into the `.returning(...)` closure.

### Integration Tests with `testcontainers`

- Clean PostgreSQL per test via `TestDatabase::new()` in `src/test/helpers/testcontainers.rs`.
- `#[tokio::test]` for async; `#[serial_test::serial]` when tests share mutable global state.
- Never hardcode DB URLs — use the container's dynamic URL.

---

## 2. Idiomatic Rust Code

### Error Handling

- `thiserror` for domain error enums; every variant has a meaningful `#[error("...")]`.
- `anyhow::Result` for internal plumbing where callers don't match on variants.
- Map errors at service boundaries with `.map_err(|e| ...)`. Don't leak internals to HTTP.
- `RequestError` pattern: domain errors `impl Responder`, map to HTTP codes.
- No `.unwrap()` / `.expect()` in production — tests and infallible ops only (e.g. compile-time regex).
- Propagate with `?` when error types align.

### Ownership and Borrowing

- Borrow (`&T`, `&mut T`) over clone. Clone only when needed; document why.
- `Arc<dyn Trait>` for shared trait objects across async boundaries. Don't use `Box<dyn Trait>` where `Arc<dyn Trait>` is established.
- `&str` over `String` in parameters when ownership isn't needed.
- `impl Into<String>` / `impl AsRef<str>` only on public APIs benefiting from flexibility.

### Types and Generics

- Concrete types over generics unless needed for testability (see §3).
- `impl Trait` in return position for iterators and closures.
- Associated types when exactly one logical type per impl.
- Enums over boolean flags (`TransactionStatus` over `is_proven: bool`).

### Patterns

- Exhaustive `match`. No `_ =>` on enums unless intentionally handling future variants.
- `if let` over full `match` for single-variant matching.
- Iterator combinators over manual loops when straightforward.
- `Option` combinators over `if let Some` chains when clearer.
- Avoid `String::from()` when `.to_string()`, `.to_owned()`, or `.into()` fits.

### Async

- `#[async_trait]` for async trait methods; trait objects crossing async boundaries must be `Send + Sync`.
- `tokio::spawn` for background tasks / workers.
- Never block the runtime: `tokio::task::spawn_blocking` for CPU-bound or blocking I/O.
- `deadpool-diesel` for async DB access via the `interact()` pattern.

#### Minimizing Async Overhead

Every `async fn` generates a state machine. Keep futures small.

- **Drop `async` if the body has no `.await`** — otherwise you get a state machine and callers forced into async context for nothing.
- **Pass-through fn: return `impl Future` instead of `async fn`.** Skip the outer state machine when forwarding a future without intermediate `.await`.
- **Widen pass-through with `futures` combinators** (`FutureExt::map`, `TryFutureExt::map_err`, `and_then`, `boxed`) to transform futures without an `async` wrapper.

  ```rust
  // Bad: async fn wraps a single forwarded call
  pub async fn get_balance(&self, addr: Address) -> Result<U256, MyError> {
      self.rpc.balance_of(addr).await.map_err(MyError::Rpc)
  }

  // Good: pass-through, no extra state machine
  use futures::TryFutureExt;
  pub fn get_balance(&self, addr: Address) -> impl Future<Output = Result<U256, MyError>> + '_ {
      self.rpc.balance_of(addr).map_err(MyError::Rpc)
  }
  ```

- **Share await points.** Consecutive independent `.await`s = one suspension each. Merge with `tokio::join!`, `futures::try_join!`, or `FuturesUnordered` for fewer suspensions and concurrency.

  ```rust
  // Bad
  let balance = rpc.balance_of(addr).await?;
  let price = prices.token_price(token).await?;

  // Good
  let (balance, price) = tokio::try_join!(rpc.balance_of(addr), prices.token_price(token))?;
  ```

- **Pass references across await points, not owned values.** Captures live in the future; moving large `Vec`/`String`/struct bloats every frame holding it. Borrow, or clone an `Arc`.

  ```rust
  // Bad: moves LargePayload into the future
  pub async fn submit(&self, payload: LargePayload) -> Result<()> { self.client.post(&payload).await }

  // Good: future holds only a reference
  pub async fn submit(&self, payload: &LargePayload) -> Result<()> { self.client.post(payload).await }
  ```

### Naming, Style, Imports

- Modules/fns/vars: `snake_case`. Types/traits/variants: `PascalCase`. Constants: `SCREAMING_SNAKE_CASE`.
- Narrowest `pub` possible. Don't `pub` items used only in the same module.
- Import types/traits by name at file top; reference bare. No inline fully qualified paths (`crate::foo::bar::Baz::new()`) — import `Baz`, call `Baz::new()`. Exception: disambiguation via `as` or one-off calls that would pollute imports.

---

## 3. Trait-Based Design for Testability

Every external dependency (DB, RPC, blockchain, queue) behind a trait. Enables mock-based unit tests, swappable implementations, clear contracts.

### Defining Traits

```rust
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait MyService: Send + Sync {
    async fn do_something(&self, input: InputType) -> Result<OutputType>;
}
```

- `#[cfg_attr(test, mockall::automock)]` generates `MockMyService`.
- Mock needed in another workspace crate's tests: `#[cfg_attr(any(test, feature = "test-features"), mockall::automock)]` + add a `test-features` feature.
- Require `Send + Sync` on async-crossing traits; use `#[async_trait]` for async methods.

### Implementing Traits

Impls are generic over their own trait dependencies:

```rust
pub struct PostgresMyRepository<D: DatabaseProvider> { db: D }

#[async_trait]
impl<D: DatabaseProvider> MyRepository for PostgresMyRepository<D> {
    async fn find(&self, id: Uuid) -> Result<MyEntity> {
        let conn = self.db.get_connection().await?;
        conn.interact(move |conn| queries::find_by_id(conn, id))
            .await
            .map_err(|e| anyhow::anyhow!("Interact error: {e}"))?
    }
}
```

### Dependency Injection

- Services take deps as `Arc<dyn Trait>` in the constructor.
- Wire concrete impls in `main.rs`; mocks in tests.
- `AppState` holds all services; inject via Rocket's `State<AppState>`.

```rust
pub struct MyService { repo: Arc<dyn MyRepository> }
impl MyService { pub fn new(repo: Arc<dyn MyRepository>) -> Self { Self { repo } } }
```

### When to Create a Trait

- **Create:** new external dependency (API, DB table, queue); service needing tests without real deps; struct calling external systems with no abstraction.
- **Don't:** pure computation without side effects; single impl with no foreseeable mocking; a simple function would do.

---

## 4. Project Conventions

- **Dependencies:** declared at workspace level in root `Cargo.toml`; crates reference with `workspace = true`. New dep → add to `[workspace.dependencies]` first.
- **Gates (must pass before merge):** `cargo fmt --all -- --check`, `cargo clippy --tests -- -Dwarnings`, `cargo test --all --workspace`.
- **Lints:** `missing_docs = "warn"` (public items need docs); `clippy::enum_variant_names = "allow"` (variants may share prefixes).

---

## 5. Code Review Checklist

- [ ] Every new public function/method has at least one test.
- [ ] External dependencies are behind traits annotated with `mockall::automock`.
- [ ] Error types use `thiserror` and map to appropriate HTTP status codes.
- [ ] No `.unwrap()` or `.expect()` in production paths.
- [ ] Async traits are `Send + Sync` and use `#[async_trait]`.
- [ ] Tests use the correct category: mocked unit tests vs. integration tests.
- [ ] New workspace dependencies are added to the root `Cargo.toml`.
- [ ] `cargo clippy --tests -- -Dwarnings` passes with no warnings.
- [ ] `cargo fmt` has been applied.
