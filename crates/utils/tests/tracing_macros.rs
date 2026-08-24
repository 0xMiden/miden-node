use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use miden_node_utils::tracing::{miden_instrument, miden_span_record};
use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt as _};
use tracing_subscriber::registry::LookupSpan;

#[derive(Clone, Default)]
struct RecordedFields(Arc<Mutex<BTreeMap<String, String>>>);

impl RecordedFields {
    fn get(&self, key: &str) -> Option<String> {
        self.0.lock().unwrap().get(key).cloned()
    }
}

impl<S> Layer<S> for RecordedFields
where
    S: Subscriber,
    for<'a> S: LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _id: &tracing::Id,
        _ctx: Context<'_, S>,
    ) {
        attrs.record(&mut FieldVisitor(self.0.clone()));
    }

    fn on_record(
        &self,
        _span: &tracing::Id,
        values: &tracing::span::Record<'_>,
        _ctx: Context<'_, S>,
    ) {
        values.record(&mut FieldVisitor(self.0.clone()));
    }
}

struct FieldVisitor(Arc<Mutex<BTreeMap<String, String>>>);

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.lock().unwrap().insert(field.name().to_owned(), format!("{value:?}"));
    }
}

#[derive(Clone, Default)]
struct RecordedEventLevels(Arc<Mutex<Vec<tracing::Level>>>);

impl RecordedEventLevels {
    fn contains(&self, level: tracing::Level) -> bool {
        self.0.lock().unwrap().contains(&level)
    }
}

impl<S> Layer<S> for RecordedEventLevels
where
    S: Subscriber,
    for<'a> S: LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        self.0.lock().unwrap().push(*event.metadata().level());
    }
}

#[miden_instrument(target = "miden-node-utils-test", name = "records_delayed_fields")]
fn records_inferred_fields() {
    let parsed_value = 42;
    let parsed_text = "parsed";

    miden_span_record!(
        block.number = parsed_value,
        transaction.id = %parsed_text,
    );
}

#[miden_instrument(
    target = "miden-node-utils-test",
    name = "records_explicit_fields",
    fields(
        account.id = tracing::field::Empty,
        account.updated = tracing::field::Empty,
    ),
)]
fn records_explicit_fields() {
    tracing::Span::current().record("account.id", tracing::field::display("explicit-account"));
    tracing::Span::current().record("account.updated", true);
}

#[miden_instrument(
    target = "miden-node-utils-test",
    name = "records_explicit_argument_field",
    fields(
        account.id = %account_id,
    ),
)]
fn records_explicit_argument_field(account_id: &str) {}

#[miden_instrument(
    target = "miden-node-utils-test",
    name = "records_explicit_and_inferred_fields",
    fields(
        account.id = tracing::field::Empty,
    ),
)]
fn records_explicit_and_inferred_fields() {
    let block_number = 9;

    tracing::Span::current().record("account.id", tracing::field::display("mixed-account"));
    miden_span_record!(
        block.number = block_number,
        transaction.id = %"mixed-tx",
    );
}

#[miden_instrument(target = "miden-node-utils-test", name = "records_fields_from_multiple_calls")]
fn records_fields_from_multiple_calls() {
    let block_number = 14;
    let tx_id = "multi-call-tx";

    miden_span_record!(block.number = block_number,);
    miden_span_record!(
        transaction.id = %tx_id,
    );
}

#[test]
fn inferred_fields_can_be_recorded_after_span_creation() {
    let recorded = RecordedFields::default();
    let subscriber = tracing_subscriber::registry().with(recorded.clone());

    tracing::subscriber::with_default(subscriber, records_inferred_fields);

    assert_eq!(recorded.get("block.number").as_deref(), Some("42"));
    assert_eq!(recorded.get("transaction.id").as_deref(), Some("parsed"));
}

#[test]
fn explicit_fields_can_be_recorded_after_span_creation() {
    let recorded = RecordedFields::default();
    let subscriber = tracing_subscriber::registry().with(recorded.clone());

    tracing::subscriber::with_default(subscriber, records_explicit_fields);

    assert_eq!(recorded.get("account.id").as_deref(), Some("explicit-account"));
    assert_eq!(recorded.get("account.updated").as_deref(), Some("true"));
}

#[test]
fn explicit_argument_fields_are_recorded_at_span_creation() {
    let recorded = RecordedFields::default();
    let subscriber = tracing_subscriber::registry().with(recorded.clone());

    tracing::subscriber::with_default(subscriber, || {
        records_explicit_argument_field("argument-account");
    });

    assert_eq!(recorded.get("account.id").as_deref(), Some("argument-account"));
}

#[test]
fn explicit_and_inferred_fields_can_be_recorded_after_span_creation() {
    let recorded = RecordedFields::default();
    let subscriber = tracing_subscriber::registry().with(recorded.clone());

    tracing::subscriber::with_default(subscriber, records_explicit_and_inferred_fields);

    assert_eq!(recorded.get("account.id").as_deref(), Some("mixed-account"));
    assert_eq!(recorded.get("block.number").as_deref(), Some("9"));
    assert_eq!(recorded.get("transaction.id").as_deref(), Some("mixed-tx"));
}

#[test]
fn multiple_span_record_macros_can_record_fields_after_span_creation() {
    let recorded = RecordedFields::default();
    let subscriber = tracing_subscriber::registry().with(recorded.clone());

    tracing::subscriber::with_default(subscriber, records_fields_from_multiple_calls);

    assert_eq!(recorded.get("block.number").as_deref(), Some("14"));
    assert_eq!(recorded.get("transaction.id").as_deref(), Some("multi-call-tx"));
}

#[miden_instrument(target = "miden-node-utils-test", name = "grpc_err_client_fault", grpc_err)]
async fn grpc_err_client_fault() -> Result<(), tonic::Status> {
    Err(tonic::Status::invalid_argument("bad request"))
}

#[miden_instrument(target = "miden-node-utils-test", name = "grpc_err_server_fault", grpc_err)]
async fn grpc_err_server_fault() -> Result<(), tonic::Status> {
    Err(tonic::Status::internal("node fault"))
}

#[tonic::async_trait]
trait GrpcErrHandler {
    async fn handle(&self, fail: bool) -> Result<(), tonic::Status>;
}

struct AsyncTraitHandler;

#[tonic::async_trait]
impl GrpcErrHandler for AsyncTraitHandler {
    #[miden_instrument(target = "miden-node-utils-test", name = "grpc_err_async_trait", grpc_err)]
    async fn handle(&self, fail: bool) -> Result<(), tonic::Status> {
        if fail {
            return Err(tonic::Status::internal("node fault"));
        }
        Ok(())
    }
}

#[tokio::test]
async fn grpc_err_client_faults_are_not_error_events() {
    let events = RecordedEventLevels::default();
    let subscriber = tracing_subscriber::registry().with(events.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    grpc_err_client_fault().await.unwrap_err();

    assert!(events.contains(tracing::Level::DEBUG));
    assert!(!events.contains(tracing::Level::ERROR));
}

#[tokio::test]
async fn grpc_err_server_faults_are_error_events() {
    let events = RecordedEventLevels::default();
    let subscriber = tracing_subscriber::registry().with(events.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    grpc_err_server_fault().await.unwrap_err();

    assert!(events.contains(tracing::Level::ERROR));
}

#[miden_instrument(target = "miden-node-utils-test", name = "grpc_err_sync", grpc_err)]
fn grpc_err_sync(fail: bool) -> Result<(), tonic::Status> {
    if fail {
        return Err(tonic::Status::internal("node fault"));
    }
    Ok(())
}

#[test]
fn grpc_err_classifies_sync_functions() {
    let events = RecordedEventLevels::default();
    let subscriber = tracing_subscriber::registry().with(events.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    grpc_err_sync(false).unwrap();
    assert!(!events.contains(tracing::Level::ERROR));

    grpc_err_sync(true).unwrap_err();
    assert!(events.contains(tracing::Level::ERROR));
}

#[tokio::test]
async fn grpc_err_classifies_async_trait_methods() {
    let events = RecordedEventLevels::default();
    let subscriber = tracing_subscriber::registry().with(events.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    AsyncTraitHandler.handle(false).await.unwrap();
    assert!(!events.contains(tracing::Level::ERROR));

    AsyncTraitHandler.handle(true).await.unwrap_err();
    assert!(events.contains(tracing::Level::ERROR));
}

#[test]
fn ui_tests() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/tracing_macros/pass.rs");
    tests.compile_fail("tests/ui/tracing_macros/invalid_field_name.rs");
    tests.compile_fail("tests/ui/tracing_macros/invalid_instrument_field_name.rs");
    tests.compile_fail("tests/ui/tracing_macros/invalid_skip.rs");
    tests.compile_fail("tests/ui/tracing_macros/invalid_skip_all.rs");
    tests.compile_fail("tests/ui/tracing_macros/outside_miden_instrument.rs");
    tests.compile_fail("tests/ui/tracing_macros/grpc_err_with_err.rs");
}
