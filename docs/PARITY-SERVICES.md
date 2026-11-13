# Services parity

Status: **in progress**. Partial entries cover only the functions named, not full source-file parity. No network credentials are embedded. Browser execution is explicitly unsupported.

## RTK

| JavaScript source | Status |
| --- | --- |
| `open-sse/rtk/filters/smartTruncate.js` | Ported: `services::rtk_filters::smart_truncate` |
| `open-sse/rtk/filters/readNumbered.js` | Ported: `services::rtk_filters::read_numbered` |
| `open-sse/rtk/filters/dedupLog.js` | Ported: `services::rtk_filters::dedup_log` |
| `open-sse/rtk/filters/tree.js` | Ported: `services::rtk_filters::tree` |
| `open-sse/rtk/filters/find.js` | Ported: `services::rtk_filters::find` |
| `open-sse/rtk/filters/grep.js` | Ported: `services::rtk_filters::grep` |
| `open-sse/rtk/filters/searchList.js` | Ported: `services::rtk_filters::search_list` |
| `open-sse/rtk/filters/gitStatus.js` | Ported: `services::rtk_filters::git_status` |
| `open-sse/rtk/filters/gitDiff.js` | Ported: `services::rtk_filters::git_diff` |
| `open-sse/rtk/filters/gitLog.js` | Not ported |
| `open-sse/rtk/filters/ls.js` | Not ported |
| `open-sse/rtk/filters/buildOutput.js` | Not ported |
| `open-sse/rtk/applyFilter.js` | Ported: `services::rtk_filters::safe_apply` |
| `open-sse/rtk/flakyGuard.js` | Partial: response classification, no stream inspection |
| `open-sse/rtk/autodetect.js` | Not ported |
| `open-sse/rtk/caveman.js` | Not ported |
| `open-sse/rtk/cavemanPrompts.js` | Not ported |
| `open-sse/rtk/constants.js` | Partial: filter constants embedded in implementations |
| `open-sse/rtk/headroom.js` | Not ported |
| `open-sse/rtk/index.js` | Not ported |
| `open-sse/rtk/registry.js` | Not ported |
| `open-sse/rtk/systemInject.js` | Not ported |
| `open-sse/rtk/ponytail.js` | Not ported |
| `open-sse/rtk/ponytailPrompt.js` | Not ported |
| `open-sse/rtk/pxpipe.js` | Not ported |

## Catalog and runtime services

| JavaScript source | Status |
| --- | --- |
| `open-sse/services/clinepassModels.js` | Ported: `services::clinepass_models` (live discovery, free-tier merge, cache, headers) |
| `open-sse/services/copilotModels.js` | Partial: catalog filtering, discovery and refresh pending |
| `open-sse/services/cursorModels.js` | Partial: protobuf catalog parsing and selection, discovery pending |
| `open-sse/services/grokCliModels.js` | Partial: model parsing, no live discovery or refresh |
| `open-sse/services/kimchiModels.js` | Partial: normalization and URL, no discovery or cache |
| `open-sse/services/kiroModels.js` | Partial: variant expansion, discovery and refresh pending |
| `open-sse/services/qoderModels.js` | Partial: PAT identification and catalog visibility |
| `open-sse/services/capacityAdapter.js` | Partial: pool selection, no history stripping |
| `open-sse/services/emptyFailover.js` | Partial: classification, no streaming peek/reconstruction |
| `open-sse/services/projectId.js` | Not ported |
| `open-sse/services/thoughtSignatureStore.js` | Not ported |
| `open-sse/services/usage.js` and `usage/*` | Partial: shared numeric and project ID helpers |
| `open-sse/services/minimaxCodeLease.js` | Partial: framing, no broker client |
| `open-sse/services/minimaxCodeUsage.js` | Partial: membership normalization only |
| `open-sse/services/nous.js` | Partial: catalog and validation helpers, no request builder |
| `open-sse/services/systemoneRouter.js` | Partial: routing helpers and sticky state |
| `open-sse/services/webIdentity.js` | Partial: cookie parsing, no probes |
| `open-sse/services/tokenRefresh.js` and `tokenRefresh/*` | Not ported |
| `open-sse/services/browserPool.js` | Partial: enable flag, browser operations unsupported |
| `open-sse/services/webLogin.js` | Unsupported |
| `open-sse/services/virtualDisplay.js` | Partial: display detection, management unsupported |
| `open-sse/services/obscura.js` | Unsupported |

## Utilities

| JavaScript source | Status |
| --- | --- |
| `open-sse/utils/awsEventStream.js` | Partial: CRC and frame decoding, no encoder |
| `open-sse/utils/claudeSignature.js` | Partial: signature validation |
| `open-sse/utils/clientDetector.js` | Ported: client detection and native pairs |
| `open-sse/utils/geminiCookies.js` | Ported: cookie normalization |
| `open-sse/utils/modelMarkers.js` | Ported: context marker stripping |
| `open-sse/utils/responsesStreamHelpers.js` | Ported: terminal detection and failure framing |
| `open-sse/utils/sse.js` | Ported: frame builders |
| `open-sse/utils/sseConstants.js` | Partial: done sentinel |
| `open-sse/utils/streamHelpers.js` | Partial: SSE parser only |
| `open-sse/utils/toolDeduper.js` | Ported: tool normalization |
| `open-sse/utils/upstreamHeaders.js` | Ported: allowed response headers |
| `open-sse/utils/webCookieAuth.js` | Partial: cookie parsing and Grok header |
| Remaining `open-sse/utils/*` | Not ported |

## Library and MITM

| JavaScript source | Status |
| --- | --- |
| `src/lib/latencyStats.js` | Ported: aggregation and database query |
| `src/lib/notify.js` | Partial: config, message and signing helpers |
| `src/lib/network/connectionProxy.js` | Partial: configuration resolution, no rotation |
| `src/lib/keyRateLimiter.js` | Partial: windows and slots, caller integration pending |
| `src/lib/providerHealthSummary.js` | Not ported |
| `src/lib/modelCatalog/*` | Not ported |
| Remaining `src/lib/network/*`, proxy pools, headroom, backup and `atRestCrypto.js` | Not ported |
| `src/mitm/antigravityIdeVersion.js` | Ported: generation request identity rewrite |
| Remaining `src/mitm/*` and tunnel helpers | Not ported |
