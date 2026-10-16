# Desktop experience
New space-app modules: app_state, targets, scan_status, hierarchy, largest_files, treemap, filters, details, native_actions. Proposed modules all live under new crates/space-app/src. GPUI entities own small UI state, not the complete scan.

## First launch and reopening
First launch shows local/removable drives, capacity/free and Choose Folder. Reopen last published scan from cache on later launch without starting a scan. Show scan interval, target, coverage, metric and disconnected/stale state. Live file actions remain disabled until the selected volume and selected-directory object identity are revalidated. Refresh is explicit. During refresh label the displayed old result and offer a separate working preview; never mix generations without labeling. Preview lists are stable rather than jumping on every file arrival.

## Views
Folders: virtualized size-sorted expandable hierarchy, breadcrumb/back/up controls, percentage/size bars, file and directory counts, unknown coverage indicators. Largest Files: paged sortable rows with charged size, path and modification date, switch between full scan and selected subtree. From file, Show in hierarchy; from folder, Largest files here. Hardlink aliases share one identity group as defined in scan policy.

Treemap follows functional list views. One deterministic layout for current folder and metric, synchronized selection, click to drill, breadcrumb to return, tooltip/details with full path and value. Restrict rendering to a bounded hierarchy depth/node count, grouping tiny entries into a labeled Other tile that can be drilled into; never silently omit their area. Unknown allocated sizes have a separate unknown indicator, not arbitrary area. Empty/zero-size datasets render a clear empty state. Use labels, focusable list alternatives and non-color-only selection; chart is not the only accessible path.

Global view state: published ScanId, subtree, metric, sort, filter and selection. All views share the same filter scope and count/byte semantics. Filters: literal name/path, extension, minimum/maximum size, modification-date range. Store filter configuration separately from scan exclusion policy. Show number of matching items and whether totals are filtered. Reset is one action.

## Native actions and safety
Copy display/native representable path to clipboard; make lossy conversion visible. Reveal only after checking current root identity and path existence, using OS APIs/argument arrays without shell interpolation. If file moved or root disconnected, explain and keep cached metadata available. Do not open file contents, run file-associated executables, delete files, or mutate source permissions. A future cleanup feature needs its own design and permission boundary.

## Acceptance
Real native tests cover keyboard-only root choice, scan/cancel/rescan, filter changes during query, old events arriving after cancellation, close/reopen during scan, disconnected drive cached browse, bad permissions, selection sync, resize/HiDPI, clipboard, reveal and screen reader/focus. Last complete scan survives every interruption. Invalidated views do not display stale search responses. Target UI response <=100 ms p95 on declared million-row benchmark; virtualized rows and treemap cap remain bounded. Export, cleanup and background watchers are outside v1.
