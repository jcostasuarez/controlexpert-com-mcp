# controlexpert-com-mcp

An MCP server for the COM automation API (UDE, Unity Developer's Edition) of **EcoStruxure Control Expert 15.1**, written from scratch in Rust with **RMCP 3.5.1** and driven by `control_expert_api_catalog.json`.

> **Unofficial project.** It is not affiliated with, endorsed by or supported by Schneider Electric. EcoStruxure and Control Expert are trademarks of their respective owners and are used here only to describe compatibility.

The executable embeds the catalog: 1,277 methods, property accessors and callbacks from 120 interfaces and 17 type libraries. The bridge can prepare **1,178 IDispatch contracts** in this version. The 54 callbacks, 41 members of interfaces without IDispatch and 4 contracts with unimplemented types can be inspected but not invoked. That count describes transport coverage. It does not mean that all 1,178 operations were tested against Control Expert, or that every interface is a supported public API.

## Download

Each [GitHub release](https://github.com/jcostasuarez/controlexpert-com-mcp/releases) has a zip with `controlexpert-com-mcp.exe` for 64-bit Windows, this README, the changelog and the example `mcp.json` and `codex-mcp.toml`, plus a `.sha256` file to check the download. Extract it anywhere and point the `command` of your MCP client entry at the extracted `controlexpert-com-mcp.exe`. The executable needs no Rust toolchain or Visual C++ runtime; COM calls still need Control Expert 15.1 installed and registered. Changes between versions are listed in [`CHANGELOG.md`](CHANGELOG.md).

## Build and run

Requirements: Rust 1.90 or later, Windows, and Control Expert 15.1 for COM calls. The catalog and its queries work without Control Expert. Python is not needed to run the server.

```powershell
cargo build --locked --release
.\target\release\controlexpert-com-mcp.exe --version
.\target\release\controlexpert-com-mcp.exe --ce-version 15.1
```

The last command starts the **stdio** transport and waits for MCP messages. `stdout` carries only JSON-RPC; diagnostics go to `stderr`. `--ce-version` rejects any version other than `15.1`.

`mcp.json` contains the stdio configuration for the executable built in this folder. Add its `mcpServers.control-expert` entry to the MCP client configuration and set `command` to the path of your built executable. This project never changes a client's global configuration by itself.

The `env` block includes the standard folders `CommonProgramFiles`, `CommonProgramFiles(x86)`, `CommonProgramW6432`, `ProgramFiles`, `ProgramFiles(x86)` and `ProgramW6432`. Some MCP clients filter inherited variables, and CE 15.1 uses the common folders during its integrity check, so they must be present. The example values match a default 64-bit Windows installation; adjust them if Windows lives on another drive or location. Each variable keeps its Windows meaning; `CommonProgramFiles` points to the native 64-bit folder.

In Codex, set the same values under `[mcp_servers.control-expert.env]`, or forward the six variables with `env_vars` in `[mcp_servers.control-expert]` if the Codex process already receives them. The [official MCP documentation](https://developers.openai.com/codex/mcp) describes both options. Restart the MCP client after changing its environment; running processes keep the environment they started with.

`codex-mcp.toml` contains a complete example entry; replace `C:/path/to/controlexpert-com-mcp` with your checkout folder. Adding it once to the user's global `config.toml` (`$CODEX_HOME/config.toml`, usually `~/.codex/config.toml`) shares it across local projects, unless project configuration or policies override it. The example file is not loaded automatically because it lives in the repository. For another user or machine, copy the block into their configuration and adjust the paths; Control Expert 15.1 must be installed and registered on the Windows machine that runs the MCP.

If **Integrity check failure** appears, read `C:\ProgramData\Schneider Electric\CommonControlExpert\IntegrityCheck.err`. In the 2026-10-18 diagnosis the log showed `\Schneider Electric Shared\COMMON\IntegrityCheckHash.dll` with `0x80092003` (`CRYPT_E_FILE_ERROR`): the base folder was missing. The installed DLL had a valid signature and the MCP client environment omitted the common folder variables; including them allowed an empty CE 15.1 session to start. A file access error does not prove an invalid signature. Check the path and the server environment before changing the installation.

Distribution only needs the executable for the catalog and the protocol; automation requires Schneider's COM components to be installed and registered. The local broker works from the 64-bit Windows executable. Some auxiliary coclasses may be 32-bit in-process components: their activation depends on the available registration and architecture, and may return `REGDB_E_CLASSNOTREG`.

## Tools

| Tool | Use |
|---|---|
| `ce_info` | Version, coverage, execution policy, workflow and limitations. |
| `ce_catalog_list` | Paginated list of libraries, interfaces, coclasses or enumerations. |
| `ce_api_search` | Searches operations by words and/or interface. |
| `ce_api_describe` | Returns the full contract, transport availability and execution policy. |
| `ce_interface_describe` | GUID, members, location and indicative access path of an interface. |
| `ce_enum_describe` | Exact names and values of an enumeration. |
| `ce_probe` | Reads the installed product version without opening projects. |
| `ce_runtime_status` | Phase, duration and work queue; answers without calling COM even while an open is busy. |
| `ce_session_start` | Starts headless, validates `IsVisible=0` and returns the broker and application; can open a given project. |
| `ce_viewer_open` | Opens, on request, a read-only view of the agent's own session. |
| `ce_object_create` | Activates a catalogued coclass and returns a handle. |
| `ce_api_call` | Invokes methods or accessors with named parameters. |
| `ce_screens_export` | Exports the operator screens of a functional module to `.XFM` and summarizes them. |
| `ce_screens_import` | Replaces the operator screens of a screens-only functional module with an edited `.XFM`. |
| `ce_object_query` | Gets a secondary interface by IID and returns a new handle. |
| `ce_objects` | Lists the COM references held by this process. |
| `ce_object_release` | Releases one handle. |
| `ce_reset` | Releases every handle. |

The tool set is small and stable. Apart from the two screen tools, which wrap a sequence the catalog cannot express safely (see [Operator screens](#operator-screens)), contracts are discovered on demand instead of publishing 1,277 separate tools or inventing a JSON signature from a natural-language description. The original description and location stay available for every operation.

Tool annotations follow what each tool can do: catalog and status tools are read-only; `ce_api_call`, `ce_object_create` and `ce_screens_import` are destructive and `ce_api_call` is open-world (it can reach a controller); `ce_object_release` and `ce_reset` are marked destructive because releasing the last reference lets Control Expert end the session, and unsaved changes may be lost.

## Usage from an MCP client

1. Call `ce_info` and `ce_probe` without arguments.
2. Search for a contract:

```json
{
  "name": "ce_api_search",
  "arguments": {"query": "Variables", "interface": "UnityProServer.IProject"}
}
```

3. Start an empty application with `ce_session_start({})`, or open a project:

```json
{
  "name": "ce_session_start",
  "arguments": {"file_path": "C:\\projects\\MyProject.stu"}
}
```

The response includes `application.$object` and `broker.$object`. Handles belong to that MCP process; they are not pointers, do not survive restarts and are not object paths.

4. Read a property with the returned handle:

```json
{
  "name": "ce_api_call",
  "arguments": {
    "handle": "<application.$object>",
    "api_id": "UnityProServer.IApplication.Version:get",
    "arguments": {}
  }
}
```

5. Get the project through `UnityProServer.IApplication.Project:get`. If a project is open, `return.$object` then lets you call `UnityProServer.IProject.Variables:get`. Objects are obtained through successive calls; the catalog access paths are indicative examples, not executable expressions.

6. Describe an operation before modifying the project:

```json
{
  "name": "ce_api_describe",
  "arguments": {"id": "UnityProServer.IVariables.Add:method"}
}
```

`ce_api_call` takes its exact arguments, for example `sVarName` and `sTypeName`. Editing requires the state and write access that Control Expert accepts. The MCP passes the server's errors through; it does not obtain permissions or change those requirements by itself.

7. Release each handle with `ce_object_release`, or all of them with `ce_reset`. Releasing references does not save the project or run `ProjectClose`. The final application lifecycle is up to Control Expert.

### Operator screens

The UDE has no operator screen API. Screens only travel as `<IOScreen>` XML (schema `SrcXmlSchema\IOScreen.xsd` in the Control Expert folder) inside functional module (`.XFM`) and project (`.XEF`) exports. Two behaviors of Control Expert 15.1, checked on a test project, shape these tools:

- `IFctModules.ImportWithStrategy` with `psrImportOptionOverwrite`, on the collection or on the module, **does not replace** existing screens: it keeps the original and adds a renamed copy (`Copia de Pantalla`).
- `IFctModule.Delete` deletes the module **and everything attached to it**, screens included. Deleting first and then importing restores the screen with its name and number.

So the workflow keeps operator screens in a dedicated top-level functional module that holds nothing else:

1. `ce_screens_export({"handle":"<application.$object>","module":"HMI_SCREENS","file_path":"C:\\work\\screens.XFM"})` writes the file and returns each screen with its attributes, object counts by `objectID` and linked variables (`varAnim`, `varPilot`). `include_objects: true` adds every object with its raw `description`. `importable` says whether the module can be replaced.
2. Edit the `.XFM`. Object `description` strings use a proprietary encoding (geometry, style, then texts between `|` separators); change texts and variables and keep the structure. `FMId`, `location` and dates are regenerated on import.
3. `ce_screens_import({... ,"file_path":"C:\\work\\screens.XFM","dry_run":true})` runs every check and writes the backup without changing anything. Without `dry_run` it:
   - checks that the file describes exactly that module and only its screens;
   - gets the project with `lWriteAccessToken=1` (or uses `project`), exports the current module to `backup_path` (default: `%TEMP%\controlexpert-com-mcp`) and refuses with `not_screens_only` if that export holds anything else (sections, variables, tasks, submodules), leaving the module untouched;
   - deletes the module, imports the file and, if the import fails, imports the backup back (`restored_from_backup` in the error);
   - re-exports the module and compares the screens with the file (`verified`, `differences`). It saves only with `save: true` and a verified result.

Control Expert reserves write access to one reference: if the client already holds an `IProject` obtained with `lWriteAccessToken=1`, pass it as `project`, or the import fails with "write access already reserved". Only top-level modules are supported. The tools release every intermediate handle. `IApplication.ImportProject` (whole `.XEF`) failed with `0xE8A72CED` in headless sessions during these tests, even with a file exported by the UI, so it is not used.

### API IDs

Each operation ID is `library.interface.member:accessor`, where the accessor is `method`, `get`, `put` or `putref`. Getters and setters have separate entries. Callbacks of event interfaces keep their accessor suffix but have `kind: "event"` and cannot be invoked.

## Execution policy

The policy is fixed when the process starts and is configured through environment variables **of the MCP server**. Only the exact value `1` unlocks a capability. Missing, `0`, empty or unknown values keep it blocked. No MCP tool can change the policy at runtime; every change requires restarting the server.

| Capability | Operations | Default | Unlock |
|---|---|---|---|
| Build | `IProject` / `IProject2`: `Build`, `BuildAll` | Blocked, also in simulation | `CE_MCP_ALLOW_BUILD=1` |
| PLC connection | `IApplication.TargetConnectionOpen` with a non-simulator target; `IApplicationSafety.SafetyTargetConnectionOpen`; `IPServerDtm` / `IPServerDtmRoot` `Connect` | Blocked | `CE_MCP_ALLOW_PLC_CONNECTION=1` |
| Simulator connection | `IApplication.TargetConnectionOpen` with `target=psrConnectToSimulator` | Allowed | — |
| PLC write | `TransferAllPCtoPLC`, `RestoreDataToPLC`, `IApplication.TargetSendCommand` (run/stop/init) to a non-simulator or unverifiable target; `IApplicationSafety.SwitchToSafetyMode` / `SwitchToMaintenanceMode` | Blocked | `CE_MCP_ALLOW_PLC_WRITE=1` |
| Simulator write | The same transfers and commands after live verification of the simulator target | Allowed | — |
| Security administration | Every operation, coclass and interface of the `security` and `security_support` categories (`UnitySecurityServer`, `CsConfigMgr`): users, profiles, policies, certificates | Blocked | `CE_MCP_ALLOW_SECURITY_ADMIN=1` |
| Private APIs | Categories `private`, `internal` and `tests` | Blocked | `CE_MCP_ALLOW_PRIVATE_APIS=1` |
| Process termination | `PSExeLib.IHealthServerGenMulti.KillProcess`, `PSExeLib.IPSEActiveConnector.ExitPServer` | Always blocked | None |
| Analysis | `Analyze`, build results and state, `TransferAllPLCToPC`, `SaveDataFromPLC` | Allowed with a valid session | — |

The capabilities are independent. Setting them in the `env` block of the MCP entry avoids unlocking other processes globally. The restricted configuration uses:

```json
"env": {
  "CE_MCP_ALLOW_BUILD": "0",
  "CE_MCP_ALLOW_PLC_CONNECTION": "0",
  "CE_MCP_ALLOW_PLC_WRITE": "0",
  "CE_MCP_ALLOW_SECURITY_ADMIN": "0",
  "CE_MCP_ALLOW_PRIVATE_APIS": "0"
}
```

Version 0.2.0 renamed the variables. The former `SCE_ALLOW_BUILD` and `SCE_ALLOW_PLC_TRANSFER` are no longer read: a configuration that still uses them keeps everything blocked.

The policy error is `operation_blocked`, with the API, reason and corresponding variable. `ce_info.execution_policy` publishes the effective policy and `ce_api_describe.execution_policy` shows the restriction of each contract; `invocable` still describes transport capability, not permission. Restricted categories also apply to `ce_object_create` (by coclass category) and `ce_object_query` (by interface category). Every check runs before the COM call.

**Connection target.** `TargetConnectionOpen` names its target explicitly, so the check reads the `target` argument: `psrConnectToSimulator` (or `1`) needs no unlock; any other value requires `CE_MCP_ALLOW_PLC_CONNECTION=1`.

**Simulator verification for writes.** Without `CE_MCP_ALLOW_PLC_WRITE`, a transfer or PLC command is allowed only after a live check, immediately before the invocation. For project transfers, the MCP compares the COM identity of the project with `IApplication.Project` of the retained applications and checks the owning application; `TargetSendCommand` checks its own application. The application must report `SelectTarget=psrConnectToSimulator` and must not have been connected to a PLC by this server: once this server opens a PLC connection on an application, switching `SelectTarget` back to the simulator does not authorize writes until `TargetConnectionClose` runs through the MCP. If the owner is not found, a read fails or the value is unknown, the write is blocked. Temporary verification references are released. The check does not see connections or changes made concurrently by the user or other clients in the CE UI.

Without `CE_MCP_ALLOW_BUILD`, a simulator transfer can only send an application whose generated code already exists. The policy covers the operations listed above; it is not a lock on every command, variable write or online change available in the catalog.

## Headless sessions and viewer

Since 0.1.3, every new application is accepted only if it confirms `Version=15.1` and `IsVisible=0`. The broker starts the application headless; `ce_session_start` returns `headless: true` after validating it. A visible application, or one whose visibility cannot be determined, produces `headless_required` and the new references are released. External windows are never hidden or closed. The same validation covers the return values of the broker's open methods and the `IApplication` references obtained by creation or QueryInterface. Direct activation of `UnityProServer.Application` and `PSExeLib.PSEConnector` is blocked: use the broker.

To watch the agent's changes, explicitly call `ce_viewer_open({"handle":"<application.$object>"})`. It invokes `IApplication.DisplayStart` with the catalogued value `psrHmiReadOnly` and an empty command line. It opens the UI of **that same session**, not another copy of the file: the view shares the in-memory project and the agent keeps its COM access. It can be opened before creating a project too. It does not open automatically when the session starts, and closing the view from CE is not a request to close the agent's session. Keep its references while working.

Direct calls to `DisplayStart` are only accepted with `psrHmiReadOnly` and `comandLine=""`. `DisplayStartControl`, `DisplayStartForMultiWriter`, the `DisplayEditor*` shortcuts and other `Display*` methods, and `SetDisplayPosition` are blocked because they could show UIs without guaranteeing read-only mode. No environment variable enables an editable UI. Secondary references to an already validated application can still be obtained while its viewer is open; a new broker open that returns a visible application is rejected.

Schneider distinguishes the **HMI R/O** and **HMI R/W** UI modes in its [FAQ about UI permissions](https://www.se.com/us/en/faqs/FA198374/). The lock applies to the MCP's use and the display parameters it sends; it does not prevent the user from opening other instances manually.

## Application gate, diagnostics and shutdown

Business operations are blocked with `application_not_ready` until this MCP client holds a `UnityProServer.IApplication` handle whose `Version` property confirmed 15.1. The validation applies to every way of obtaining that interface (returns, creation and QueryInterface). The window does not need to be shown: a headless session meets the same condition. A CE process or window started outside does not enable this client.

The catalog, `ce_info`, `ce_probe`, `ce_runtime_status`, `ce_objects`, `ce_object_release` and `ce_reset` remain available. Before CE is open, `ce_object_create` only allows `UnityProBroker.PServerBroker` and `UnityProBroker.PServerBrokerContext`; calls and interface queries only allow `IPServerBroker`, `IPServerBrokerContext`, `IPSBBrokerInfo` and `IHealthServerGen` of `UnityProBroker`, so the session can be started and diagnosed. Every other activation, call and QueryInterface is rejected before invoking COM. The tools stay in the MCP catalog, with the gate applied on the STA when each request runs.

`ce_runtime_status` and `ce_objects` publish `application_ready`. It means a validated application reference is still held, not a continuous liveness check of the external process. Releasing the last `IApplication` or running `ce_reset` blocks business operations again, even if child handles remain; releasing those children is always allowed. A failed or cancelled open releases its new references and does not grant access. An additional failed open does not block other valid applications this client already holds.

`ce_probe` checks the installation and version of `pserver.dll`. Its `compatible` field refers to that version, not to the startup state of Control Expert. It returns `application_activation_tested: false` and `compatibility_scope` to make that scope explicit. Real activation is checked when a session starts.

`ce_session_start` activates the broker and opens or creates the application in a single STA operation. It returns the broker and application handles plus `timings_ms` with `broker_activation`, `application_open_or_create` and `total`. If a stage fails, the new references of that attempt are released. If the client cancels during COM or drops the result before receiving it, those references are released when the call finishes. This does not undo changes or guarantee that Control Expert closes its process.

If startup returns `0x80080005` (directly or inside `details.exception_scode`), the error adds `details.activation_diagnostic` with recovery steps and keeps the original codes, message and data. This also applies to direct calls to the `NewApplication*` and `OpenApplication*` methods of `IPServerBroker`. Nothing is retried and no UI is opened automatically. The diagnostic separates the failure of the external server from a specific cause that is still unknown.

After that failure, first wait until `ce_runtime_status.active_operation` is `null`. Then open **Control Expert Classic** manually and let its initialization finish; review errors or dialogs and complete the DTM catalog update if it asks for one. Retry the requested open once after that startup completes. Schneider describes a possible 15.1 cause related to DTM catalog initialization in its [FAQ000234251](https://www.se.com/au/en/faqs/FAQ000234251/); that reference does not confirm the cause on every machine. If it persists, review the DistributedCOM events and the Control Expert logs. Do not change DCOM permissions or terminate processes as recovery.

A session created while Classic is open verifies that state; to confirm recovery of a cold start, also test without a previously started instance. The `--live --session` smoke test creates an empty application and reads its version and project state, but does not close other instances to prepare that test.

If Classic opens manually but activation fails from an isolated process, compare the same binary and arguments inside and outside that isolation, with the corresponding authorization. In the local diagnosis of 2026-10-18, the same `--live --session` smoke failed with `0x80080005` from the isolated terminal and passed outside it in 4.3 seconds. That points to a difference in the execution environment; it does not identify which restriction prevented the start. Do not confuse it with an invalid COM signature or attribute it automatically to the DTM catalog.

Errors and cancellations do not release references the client already received: doing so could interrupt an edit with unsaved changes. The client must use `ce_reset` in a `finally` block, even when a business call fails. To close a project deliberately, save first and release its child objects; then invoke `IApplication.ProjectClose`. Do not close other clients' projects to resolve a write reservation.

Client disappearance is detected when stdio closes. The executable stops accepting work, drops pending operations that have not started, waits for the active COM call and releases references from newest to oldest on the STA, before `CoUninitialize`. The main process waits for the COM thread to finish even if the MCP service fails. Killing the process from the operating system prevents this cleanup.

When an open takes long, query `ce_runtime_status({})` from the same client; this tool does not go through the COM queue. It shows `active_operation`, `phase`, `elapsed_ms`, `phase_elapsed_ms` and `queued_operations`. Do not start a second open to check whether the first one finished. `ce_objects` goes through the STA and can therefore wait behind the open.

The server writes JSON events to `stderr` with operation ID, phase, duration and error codes, without logging paths or argument values. It logs the `broker_activation`, `application_create` or `application_open` phases. Errors also include `details.runtime` with the failed phase. `stdout` stays reserved for JSON-RPC.

Every 10 seconds of waiting it emits `operation_waiting`; during a pending shutdown, `runtime_shutdown_waiting`. The interval can be adjusted:

```powershell
.\target\release\controlexpert-com-mcp.exe --ce-version 15.1 --slow-call-ms 5000
```

This interval is diagnostic: it does not cancel COM or limit the operation. If a COM call blocks indefinitely, the orderly shutdown also keeps waiting. A client timeout does not prove that the operation failed. Query `ce_runtime_status` before deciding anything else.

Symbolic enum names are converted only when the signature identifies the enumerated type. For `VARIANT` parameters, check `ce_enum_describe` and send the number or its explicit representation. For example, `ITasks.Item` takes `EnumTaskName` as a `VARIANT`: for MAST, the `UnityProServer.epsrTaskType` catalog entry gives `PSR_MAST_TASK = 4`, so send `{"EnumTaskName":4}`. Sending `"PSR_MAST_TASK"` produces a BSTR, not an enum.

## I/O conventions

A successful call returns:

```json
{
  "api_id": "UnityProServer.IProject.GetVersion:method",
  "return": "...",
  "outputs": {
    "plVersionAutoIncrementset": 0,
    "plBuildVersion": 0,
    "plMajorVersion": 0,
    "plMinorVersion": 0
  }
}
```

The example shows the structure, not a real project version. `[out]` parameters are allocated by the server: the client must not send them. `[in,out]` parameters are sent and also appear in `outputs`. Input parameters passed by reference keep their storage and their updates are included in `outputs`, even when the type library does not mark them as outputs.

Parameters whose direction is missing in the type library are treated as inputs. They are not assumed to be outputs: if the contract requires a `VARIANT*` with unspecified direction, it can be initialized explicitly with `{"$variant":"empty"}` and read back from `outputs`.

Optional parameters can be omitted. The type library default is used when it exists; otherwise `DISP_E_PARAMNOTFOUND` is sent. A default of `0` for an interface reference becomes a null reference. `null`, `empty` and `missing` mean different things in COM.

| JSON input value | COM value |
|---|---|
| String, boolean, integer or decimal | Type required by the contract, with range checks. |
| Symbolic name of an enum | Numeric value of that enum, without name-based guesses. |
| `{"$object":"handle"}` | Retained COM reference; the required IID is queried when needed. |
| JSON array inside a VARIANT | `SAFEARRAY(VARIANT)` with lower bound 0. |
| `{"$variant":"bytes","value":[0,127,255]}` | `SAFEARRAY(BYTE)`. |
| `{"$variant":"array","value":[1,"text",true]}` | `SAFEARRAY(VARIANT)`. |
| `{"$variant":"empty"}` | `VT_EMPTY`. |
| `{"$variant":"null"}` or `null` in a VARIANT | `VT_NULL`. |
| `{"$variant":"missing"}` | `VT_ERROR / DISP_E_PARAMNOTFOUND`. |
| `{"$variant":"i2|i4|ui4|i8|r8","value":42}` | Explicit scalar type for a VARIANT. |

In the last row pick **one** type name, for example `"i4"`; the bars only summarize the options. JSON `null` is accepted for a null interface reference in a typed object parameter.

Outputs support scalars, references, `VARIANT` and `SAFEARRAY` of up to four dimensions. Zero-based one-dimensional arrays come out as JSON arrays. Others keep their bounds as `{"$array": [...], "bounds": [{"lower":1,"upper":3}]}`. `VT_DATE` and VARIANT errors have a tagged representation; output `VT_EMPTY` and `VT_NULL` are JSON `null`. The maximum array size is one million elements and the maximum serialization depth is 16.

Unimplemented types are rejected with `unsupported_operation`; arbitrary pointers or native structures are never reinterpreted. A `VARIANT` contract alone does not reveal the internal format a specific API expects.

## Architecture and future versions

```text
MCP client --stdio--> RMCP --tools--> Catalog + Policy + Runtime
                                                  |
                                          message channel
                                                  |
                                          single COM STA thread
                                                  |
                                          Schneider IDispatch
```

- `src/versions/v15_1.rs`: catalog, installation and 15.1 profile. Another version needs an explicit profile, its catalog and its tests; it is never enabled by GUID similarity.
- `src/catalog.rs`: search, alias/enum resolution, coverage and parameter validation.
- `src/policy.rs`: classification of restricted operations and the startup execution policy.
- `src/runtime/windows.rs`: object creation, QueryInterface, Invoke, marshalling, serialization and live policy checks.
- `src/runtime/mod.rs`: operation queue. Only Rust/JSON data crosses the thread boundary; no COM pointer leaves the STA.
- `src/server.rs`: tools and RMCP metadata, independent of Windows.

Calls are serialized on an STA with a message pump. `QueryInterface` keeps the pointer of the secondary interface to invoke its members. On shutdown, objects are released before `CoUninitialize`.

Pending operations cancelled before running are discarded. A COM call that already started cannot be interrupted through this protocol: cancelling or losing a response does not prove that the project was not modified. The server never retries operations automatically.

Business failures are returned with `isError: true`, a stable code and, when COM provides them, HRESULT, EXCEPINFO description, source and the name of the failing argument. If COM finished but the result could not be serialized, the error includes `com_operation_completed: true`; references that did not reach the client are released. This does not roll back the operation. Unknown arguments are rejected.

## Catalog

`control_expert_api_catalog.json` is generated from the type libraries installed with Control Expert 15.1. Its schema (version 2.0.0) uses English keys; descriptions come either from the type library (`description_source: "typelib"`) or from interpreting the member name (`name_interpretation`), and `original_documentation` keeps the vendor text.

```powershell
powershell -NoProfile -File scripts/extract_typelibs.ps1
python scripts/build_catalog.py
```

The first step reads type metadata only (`LoadTypeLibEx` with `REGKIND_NONE`) and the COM registration; it does not instantiate Control Expert or connect to a PLC. The second step has no third-party Python dependencies. Changing the catalog requires rebuilding the executable.

## Verification

```powershell
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
python scripts/smoke_mcp.py --exe target/release/controlexpert-com-mcp.exe
python scripts/smoke_mcp.py --exe target/release/controlexpert-com-mcp.exe --live
python scripts/smoke_mcp.py --exe target/release/controlexpert-com-mcp.exe --live --session --eof-cleanup
```

The Rust tests exercise the catalog, the execution policy and a local IDispatch test object: reversed argument order, PUT properties, by-reference parameters, outputs, arrays, enums, COM errors, handle release, connection targets, simulator verification and restricted categories. The test object does not exist as a mode of the production executable.

The lifecycle tests block a test backend to verify cancellation before and during execution, results dropped after delivery, cleanup of partial opens, preservation of previous references and concurrent or cancelled shutdown. A test with a real IDispatch checks the release order of references on Windows.

The smoke test drives the real binary over stdio: MCP initialization, discovery, contracts, policy metadata, RMCP 3.5.1 metadata and in-band errors. With `--live` it also activates the installed broker, reads `InterfaceVersion` and releases its references. It opens no project and does not connect to the PLC. With `--live --session` it also starts an application, reads its version and project open state, checks that a PLC connection, a security interface query and a security coclass are rejected before COM, and releases the references; it does not modify projects.

`--eof-cleanup` requires `--live`: it deliberately keeps a broker reference to check that closing stdin releases it. The smoke verifies that the COM thread finished before the process exits. It waits up to 300 seconds, configurable with `--request-timeout` and `--shutdown-timeout`, and does not kill the server if the shutdown wait expires. On failure it tries `ce_reset` before disconnecting.

`--live --session --viewer` also verifies the headless start (`IsVisible=0`), the rejection of the editable viewer and the explicit opening of the read-only view of the same application. It then confirms that the UI is visible and COM still responds. It uses an empty session; it does not create projects, build or transfer.

References: [RMCP 3.5.1](https://github.com/modelcontextprotocol/rust-sdk/releases/tag/rmcp-v3.5.1), [official Rust SDK](https://github.com/modelcontextprotocol/rust-sdk), [Unity Developer's Edition](https://www.se.com/sg/en/product-range/630-unity-developers-edition/).

## Code quality

The lint policy lives in `Cargo.toml` (`[lints]`) and `clippy.toml`. It is adapted from [BeautifulRust](https://github.com/morganlinton/BeautifulRust), which distills the Rust API Guidelines, *Effective Rust* and other style guides, with the library-only lints left out because this crate is an application. In short:

- every `unsafe` block holds one operation and a `// SAFETY:` comment that says why it is sound; `VARIANT` union access goes through two helpers in `src/runtime/windows.rs`;
- no `unwrap()` outside tests, no silent `as` casts, no `bool` parameters, no wildcard imports;
- a lint is silenced only with `#[expect(..., reason = "...")]` on the narrowest item, so stale suppressions are reported.

CI (`.github/workflows/ci.yml`) runs on Windows on every push to `main` and every pull request: `rustfmt`, `clippy -D warnings`, the test suite (including the COM marshalling and policy tests, which use local `IDispatch` fixtures and need no Control Expert installation) and a build with the minimum supported Rust version from `rust-version`. The Rust version for fmt, clippy and tests is pinned in the workflow (`RUST_TOOLCHAIN`) so a new Rust release cannot break CI with new lints; bump it deliberately, together with the fixes it asks for.

## Releasing

1. Move the `[Unreleased]` entries of `CHANGELOG.md` into a new section `## [X.Y.Z] - YYYY-MM-DD` and set `version = "X.Y.Z"` in `Cargo.toml` (run `cargo check` so `Cargo.lock` follows).
2. Merge to `main` with CI green.
3. Tag that commit and push the tag:

   ```bash
   git tag -a vX.Y.Z -m "vX.Y.Z"
   git push origin vX.Y.Z
   ```

`.github/workflows/release.yml` then checks that the tag matches `Cargo.toml` and that the changelog has a section for it, runs the tests, builds the release executable and publishes the GitHub release with the zip, its SHA-256 and that changelog section as notes.

## License

MIT.
