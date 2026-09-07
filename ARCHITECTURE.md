# Brother Label Printer — Proposed Application Architecture

## 1. Goals

The application should:

- Print labels reliably to supported Brother QL network printers.
- Keep the interface responsive while rendering and printing.
- Clearly distinguish validation, transmission, and confirmed-print outcomes.
- Produce an accurate preview using the same rendering path as printing.
- Support Windows and macOS as self-contained desktop builds.
- Make new label formats reasonably easy to add without prematurely building a general template language.
- Remain testable without requiring a physical printer.

## 2. Architectural style

Use a pragmatic layered architecture with ports at external boundaries:

```text
Slint UI
   │ commands / state updates
   ▼
Application services
   │ validated domain values
   ▼
Domain model
   │
   ├── Label compiler port ──▶ Brother QL rendering adapter
   ├── Printer port ─────────▶ TCP printer adapter
   └── Repository ports ─────▶ Local JSON/JSONL storage
```

Dependency rules:

1. The domain does not depend on Slint, networking, files, or `brother_ql`.
2. Application services coordinate operations but do not contain rendering or TCP details.
3. Infrastructure adapters implement external behavior.
4. The UI communicates only with an application facade.
5. `main.rs` constructs and connects concrete implementations.

This preserves useful hexagonal boundaries without creating a trait for every internal function.

## 3. Domain model

The domain owns business concepts, validation rules, and state-independent decisions.

### Identifiers and validated values

```rust
struct PrinterId(Uuid);
struct TemplateId(String);
struct TemplateVersion(u32);

struct PrinterName(String);
struct PrinterAddress(SocketAddr);
struct MacAddress([u8; 6]);
struct AssetTag(String);
struct SerialNumber(String);
struct CopyCount(NonZeroU8);
```

Constructors validate input and return typed validation errors.

Sensitive implementation details such as the original invalid value should not automatically be persisted or logged.

### Printer preset

```rust
struct PrinterPreset {
    id: PrinterId,
    name: PrinterName,
    address: PrinterAddress,
    model: PrinterModel,
}
```

A preset represents durable configuration. It does not claim that a particular roll is currently installed.

### Media

```rust
enum Media {
    DieCut(MediaDefinition),
    Continuous(MediaDefinition),
}

struct MediaDefinition {
    id: MediaId,
    printable_width_px: u32,
    printable_height_px: Option<u32>,
    physical_width_mm: Decimal,
    physical_height_mm: Option<Decimal>,
    color_mode: ColorMode,
}
```

Supported media should be defined centrally and mapped explicitly to the media types supported by `brother_ql`.

### Label templates

Begin with built-in, versioned templates:

```rust
enum LabelTemplate {
    AppleTechV1,
    AssetTagV1,
}
```

Each template provides:

- field definitions;
- validation rules;
- supported media;
- default media;
- layout specification;
- human-readable name;
- template version.

A layout specification can be declarative inside Rust without initially becoming a user-editable format:

```rust
struct LabelLayout {
    canvas: CanvasSpec,
    elements: Vec<LayoutElement>,
}

enum LayoutElement {
    Text(TextElement),
    Image(ImageElement),
    Rule(RuleElement),
}
```

This supports shared rendering while retaining compile-time control over templates.

### Draft and validated jobs

Do not represent all job states with one generic map.

```rust
struct DraftLabelJob {
    template: TemplateId,
    fields: BTreeMap<FieldId, String>,
}

struct ValidatedLabelJob {
    template: TemplateId,
    template_version: TemplateVersion,
    values: BTreeMap<FieldId, FieldValue>,
}
```

Only the application validation operation can produce a `ValidatedLabelJob`.

`BTreeMap` is preferred where deterministic ordering helps tests and serialization.

### Print request

```rust
struct PrintRequest {
    job: ValidatedLabelJob,
    printer: PrinterPreset,
    media: MediaId,
    copies: CopyCount,
    cut_behavior: CutBehavior,
}
```

## 4. Application layer

The application layer coordinates use cases and exposes one facade to the UI.

```rust
trait AppService {
    fn validate_label(
        &self,
        draft: DraftLabelJob,
    ) -> Result<ValidatedLabelJob, ValidationErrors>;

    fn preview_label(
        &self,
        job: ValidatedLabelJob,
        media: MediaId,
    ) -> Result<PreviewImage, PreviewError>;

    fn submit_print(
        &self,
        request: PrintRequest,
    ) -> Result<JobId, SubmitError>;

    fn cancel_print(&self, job_id: JobId) -> CancelResult;

    fn recent_history(&self, limit: usize)
        -> Result<Vec<HistoryEntry>, RepositoryError>;
}
```

The UI submits work and receives a `JobId` immediately. Rendering and network operations never block the UI thread.

### Background print coordinator

A `PrintCoordinator` owns a worker queue and executes one job at a time per printer.

```text
Queued
  ↓
Validating configuration
  ↓
Rendering
  ↓
Connecting
  ↓
Sending
  ↓
Awaiting status, when supported
  ↓
ConfirmedPrinted or SentUnconfirmed
```

Terminal failure can occur during any stage.

The coordinator publishes events:

```rust
enum PrintEvent {
    Queued { job_id: JobId },
    Rendering { job_id: JobId },
    Connecting { job_id: JobId },
    Sending { job_id: JobId, progress: Option<u8> },
    AwaitingConfirmation { job_id: JobId },
    Finished { job_id: JobId, result: PrintResult },
}
```

Slint callbacks submit commands. Worker events are returned to the Slint event-loop thread before UI properties are changed.

### Concurrency policy

- Serialize jobs targeting the same printer.
- Different printers may eventually use independent queues.
- Prevent duplicate submission from repeated button clicks.
- Give every request a unique job ID.
- Apply bounded connection, write, and status timeouts.
- Do not retry automatically after an ambiguous transmission failure because doing so may print duplicate labels.
- Allow automatic retry only when the application knows no print data was accepted.

## 5. External ports

Use ports only where an external system or independently replaceable implementation exists.

### Label compiler

```rust
trait LabelCompiler: Send + Sync {
    fn preview(
        &self,
        job: &ValidatedLabelJob,
        media: &Media,
    ) -> Result<PreviewImage, CompileError>;

    fn compile(
        &self,
        job: &ValidatedLabelJob,
        media: &Media,
        options: &PrintOptions,
    ) -> Result<CompiledPrintJob, CompileError>;
}
```

Preview and printer output must share the same layout calculation. This prevents the preview from disagreeing with the printed result.

```rust
struct CompiledPrintJob {
    bytes: Vec<u8>,
    metadata: CompiledJobMetadata,
}
```

### Printer transport

```rust
trait PrinterTransport: Send + Sync {
    fn inspect(
        &self,
        printer: &PrinterPreset,
    ) -> Result<PrinterStatus, TransportError>;

    fn send(
        &self,
        printer: &PrinterPreset,
        job: &CompiledPrintJob,
        observer: &dyn TransferObserver,
    ) -> Result<TransportReceipt, TransportError>;
}
```

The TCP implementation should own:

- address resolution;
- socket creation;
- timeouts;
- buffered transmission;
- shutdown behavior;
- supported status requests and parsing;
- mapping low-level errors into stable application errors.

### Repositories

```rust
trait SettingsRepository: Send + Sync {
    fn load(&self) -> Result<AppSettings, RepositoryError>;
    fn save(&self, settings: &AppSettings) -> Result<(), RepositoryError>;
}

trait HistoryRepository: Send + Sync {
    fn append(&self, entry: &HistoryEntry) -> Result<(), RepositoryError>;
    fn recent(&self, limit: usize)
        -> Result<Vec<HistoryEntry>, RepositoryError>;
    fn clear(&self) -> Result<(), RepositoryError>;
}
```

## 6. Printing outcomes and errors

A successful socket write must not automatically be called a successful print.

```rust
enum PrintOutcome {
    ConfirmedPrinted,
    SentUnconfirmed,
}

struct PrintResult {
    outcome: PrintOutcome,
    completed_at: SystemTime,
    warnings: Vec<PrintWarning>,
}
```

Recommended user-facing interpretations:

- `ConfirmedPrinted`: the printer reported successful completion.
- `SentUnconfirmed`: all bytes were transmitted, but physical completion could not be verified.
- `Failed`: printing did not reach a successful terminal state.
- `Unknown`: transmission began, but the connection failed before the result could be determined. Reprinting may create a duplicate.

### Error hierarchy

```rust
enum PrintFailure {
    UnsupportedPrinter,
    UnsupportedMedia,
    MediaMismatch {
        expected: MediaId,
        detected: Option<MediaId>,
    },
    PrinterUnavailable,
    ConnectionRefused,
    ConnectTimeout,
    WriteTimeout {
        bytes_sent: usize,
    },
    ConnectionLost {
        bytes_sent: usize,
    },
    PrinterReported(PrinterFault),
    Compile(CompileError),
}

enum CompileError {
    MissingAsset,
    FontUnavailable,
    ContentOverflow { field: FieldId },
    InvalidDimensions,
    RasterGeneration,
}

enum RepositoryError {
    Unavailable,
    InvalidData,
    UnsupportedVersion,
    WriteFailed,
}
```

Keep technical causes available for diagnostic logs, but map them to short corrective messages for operators.

History persistence failure should be returned as a warning and must not change a confirmed print into a failed print.

## 7. Infrastructure adapters

### Brother QL compiler

`BrotherQlCompiler` implements `LabelCompiler`.

Responsibilities:

1. Calculate layout in printer pixels.
2. Load packaged fonts and images.
3. Render a deterministic monochrome or two-color canvas.
4. Generate the UI preview from that canvas.
5. Pass the same canvas to `brother_ql`.
6. Compile it into raster command bytes.

Only this adapter imports `brother_ql`.

The precise crate version should be selected after testing generated output against the supported printer and media matrix.

### TCP printer transport

`TcpPrinterTransport` implements `PrinterTransport`.

It should support:

- raw TCP printing on the configured port;
- separate connect, write, and response timeouts;
- status inspection where supported;
- completion/status reading where reliable;
- exact byte-count reporting on failure;
- dependency injection of a socket factory or a local fake printer for tests.

Do not include speculative retries in the initial implementation.

### Settings storage

Use a versioned JSON file:

```json
{
  "schema_version": 1,
  "printers": [],
  "last_printer_id": null,
  "theme": "system"
}
```

Save by writing a new temporary file and atomically replacing the previous file.

Store it in the platform-appropriate application-data directory rather than beside the executable.

### History storage

Use append-only JSON Lines initially. Each entry should contain:

- job ID;
- submission and completion timestamps;
- template ID and version;
- printer preset ID and display name;
- media ID;
- copy count;
- outcome or failure category;
- warnings.

Do not persist raw label contents by default. Asset tags, serial numbers, and MAC addresses may be operationally sensitive. If operators need reprinting, make that an explicit retention decision.

## 8. UI architecture

Use Slint views plus a small Rust controller.

```text
Slint components
      │ callbacks
      ▼
UI Controller
      │ commands
      ▼
Application facade / print coordinator
      │ events
      ▼
UI Controller
      │ property updates on event-loop thread
      ▼
Slint AppState
```

### UI state

```rust
struct UiState {
    draft: DraftLabelJob,
    validation: ValidationState,
    preview: PreviewState,
    selected_printer: Option<PrinterId>,
    active_job: Option<PrintJobView>,
    history: Vec<HistoryRow>,
}
```

Recommended screen states:

- Empty form
- Invalid form
- Ready to print
- Rendering preview
- Queued
- Connecting
- Sending
- Awaiting confirmation
- Printed
- Sent but unconfirmed
- Failed with suggested action

Disable the Print button while the current request is active unless intentional queueing is later added.

### Preview behavior

- Debounce preview updates while typing.
- Render previews on a worker thread if rendering is noticeable.
- Discard stale preview results using a monotonically increasing revision number.
- Display content-overflow warnings before printing.
- Use packaged fonts so preview output remains consistent across operating systems.

## 9. Proposed project layout

```text
brother-label-printer/
├── Cargo.toml
├── build.rs
├── assets/
│   ├── fonts/
│   ├── images/
│   └── templates/
├── src/
│   ├── main.rs
│   ├── lib.rs
│   ├── domain/
│   │   ├── mod.rs
│   │   ├── label.rs
│   │   ├── media.rs
│   │   ├── printer.rs
│   │   ├── template.rs
│   │   └── validation.rs
│   ├── application/
│   │   ├── mod.rs
│   │   ├── app_service.rs
│   │   ├── print_coordinator.rs
│   │   ├── print_state.rs
│   │   └── ports.rs
│   ├── infrastructure/
│   │   ├── mod.rs
│   │   ├── compiler/
│   │   │   ├── mod.rs
│   │   │   ├── brother_ql.rs
│   │   │   ├── layout.rs
│   │   │   └── fonts.rs
│   │   ├── printer/
│   │   │   ├── mod.rs
│   │   │   ├── tcp.rs
│   │   │   └── status.rs
│   │   └── storage/
│   │       ├── mod.rs
│   │       ├── settings_json.rs
│   │       └── history_jsonl.rs
│   ├── ui/
│   │   ├── mod.rs
│   │   ├── controller.rs
│   │   ├── models.rs
│   │   ├── app.slint
│   │   └── components/
│   └── platform/
│       ├── mod.rs
│       ├── paths.rs
│       └── resources.rs
├── tests/
│   ├── fixtures/
│   │   ├── expected_raster/
│   │   └── fonts/
│   ├── compiler_contract.rs
│   ├── fake_printer.rs
│   ├── print_coordinator.rs
│   └── persistence.rs
└── xtask/
    └── packaging and verification tools
```

Create directories only when their first implementation is introduced.

## 10. Testing strategy

### Domain tests

Test without UI, files, network, or printer libraries:

- value-object parsing;
- field validation;
- supported template/media combinations;
- draft-to-validated-job conversion;
- copy-count boundaries.

### Compiler contract tests

For each supported template and media:

- render known inputs;
- compare dimensions and pixel characteristics;
- compare compiled raster bytes with reviewed fixtures where stable;
- verify long-field overflow behavior;
- verify packaged-font determinism;
- verify preview and print layouts use the same canvas.

Image snapshots should be reviewable artifacts, not opaque assertions.

### Transport tests

Implement a local fake printer server that can:

- accept a complete job;
- refuse connections;
- delay reads;
- close after a specified byte count;
- return valid printer status;
- return printer errors;
- never return completion.

This provides deterministic coverage without hardware.

### Hardware qualification tests

Maintain a manual compatibility matrix:

| Printer      | Firmware | Connection | Media      | Compile   | Send      | Status    | Completion            |
| ------------ | -------- | ---------- | ---------- | --------- | --------- | --------- | --------------------- |
| Target model | Recorded | TCP        | Target SKU | Pass/Fail | Pass/Fail | Pass/Fail | Confirmed/Unavailable |

Run this qualification before releases that change the compiler or transport.

### UI tests

Keep most state-transition tests in Rust. Use Slint’s testing backend for a small set of critical interactions:

- invalid input disables printing;
- Print submits exactly one request;
- worker events update visible status;
- ambiguous outcomes do not display “Printed”;
- manual address and preset selection behave correctly.

## 11. Observability and support diagnostics

Use structured local diagnostics with:

- timestamp;
- job ID;
- stage;
- printer ID;
- destination address;
- duration;
- bytes generated;
- bytes sent;
- outcome category;
- error chain.

Do not log label contents by default.

Provide a “Copy diagnostics” action that creates a sanitized summary suitable for a support ticket. Log files should rotate and have a bounded total size.

## 12. Delivery sequence

### Phase 1 — Hardware proof

Build a command-line diagnostic path that renders and sends one fixed test label.

Exit criteria:

- exact target printer and media identified;
- raster output physically verified;
- timeout behavior understood;
- completion/status capability documented;
- `brother_ql` version selected using evidence.

### Phase 2 — Core vertical slice

Implement:

- one built-in template;
- validated job types;
- shared preview/print rendering;
- TCP printing;
- asynchronous print coordinator;
- honest outcome reporting;
- fake-printer integration tests.

Exit criterion: a user can enter one label, preview it, print it, and understand the result.

### Phase 3 — Desktop usability

Implement:

- polished Slint form;
- printer presets;
- manual address;
- settings persistence;
- history;
- sanitized diagnostics;
- packaged fonts and assets;
- Windows and macOS packaging.

### Phase 4 — Additional workflows

Add only after observing actual use:

- additional built-in templates;
- batch queue;
- CSV mapping and preview;
- reprint workflow;
- printer discovery;
- customizable templates;
- USB support.

## 13. Explicitly deferred features

The first release should not include:

- a general-purpose user-editable template language;
- SQLite unless query or scale requirements justify it;
- automatic retries after ambiguous partial transmission;
- printer discovery;
- USB support;
- arbitrary plugins;
- CSV batch printing;
- storage of complete label contents by default.

These remain compatible with the architecture but should not delay proving reliable single-label printing.

## 14. Key architectural decisions to record

Create short architecture decision records for:

1. Supported printer models and media.
2. Selected `brother_ql` version.
3. Meaning of “Printed” versus “Sent.”
4. Packaged font and rendering engine.
5. TCP status and completion strategy.
6. Label-data retention policy.
7. Configuration and history storage locations.
8. Windows and macOS packaging approach.

## 15. Definition of a reliable first release

The first release is ready when:

- the preview and physical label use the same layout;
- the UI never blocks during printing;
- duplicate prints are not caused by automatic retry;
- network and printer errors produce actionable messages;
- unconfirmed transmission is never presented as confirmed printing;
- configuration writes are crash-safe;
- sensitive label data is not logged unexpectedly;
- the supported hardware matrix has been physically tested;
- Windows and macOS packages run without separately installed runtimes.
