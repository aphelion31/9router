# Rewrite gap report (REA)

Generated with [REA](https://github.com/morluto/rea) `analyze-javascript-application` on 9router-pro (1,694 JS modules, 1,375 functions, 1.28M AST nodes),
compared with `origin/develop` Rust sources by module and function name. Name matching is heuristic: a listed module can exist in Rust under another name,
so each row needs a manual check. Dashboard React components are excluded (the Rust dashboard is plain HTML/JS).

Modules below 50% name coverage: **283**.

## Media handlers (tts/stt/image/video/embeddings) (21)

| JS module | functions | name coverage |
|---|---|---|
| `open-sse/config/googleTtsLanguages.js` | 0 | 0% |
| `open-sse/config/mediaConfig.js` | 0 | 0% |
| `open-sse/config/ttsModels.js` | 7 | 0% |
| `open-sse/handlers/codexStt.js` | 0 | 0% |
| `open-sse/handlers/codexSttAudio.js` | 0 | 0% |
| `open-sse/handlers/embeddingsCore.js` | 0 | 0% |
| `open-sse/handlers/geminiLiveStt.js` | 0 | 0% |
| `open-sse/handlers/imageGenerationCore.js` | 0 | 0% |
| `open-sse/handlers/responsesHandler.js` | 0 | 0% |
| `open-sse/handlers/sttCore.js` | 0 | 0% |
| `open-sse/handlers/systemoneCore.js` | 0 | 0% |
| `open-sse/handlers/ttsCore.js` | 0 | 0% |
| `open-sse/handlers/ttsProviders/edgeTts.js` | 0 | 0% |
| `open-sse/handlers/ttsProviders/genericFormats.js` | 0 | 0% |
| `open-sse/handlers/ttsProviders/googleTts.js` | 0 | 0% |
| `open-sse/handlers/ttsProviders/index.js` | 0 | 0% |
| `open-sse/handlers/ttsProviders/selfhostedTts.js` | 0 | 0% |
| `open-sse/handlers/videoCore.js` | 0 | 0% |
| `open-sse/handlers/videoProviders/index.js` | 0 | 0% |
| `src/sse/handlers/stt.js` | 0 | 0% |
| `src/sse/handlers/tts.js` | 0 | 0% |

## Executor support (protocols, auth, TLS) (50)

| JS module | functions | name coverage |
|---|---|---|
| `open-sse/config/codexInstructions.js` | 0 | 0% |
| `open-sse/executors/antigravity.js` | 33 | 29% |
| `open-sse/executors/azure.js` | 5 | 0% |
| `open-sse/executors/base.js` | 16 | 14% |
| `open-sse/executors/bedrock.js` | 19 | 10% |
| `open-sse/executors/chatgpt-web.js` | 12 | 20% |
| `open-sse/executors/chatgpt-web/adapter.js` | 27 | 10% |
| `open-sse/executors/chatgpt-web/attachments.js` | 16 | 0% |
| `open-sse/executors/chatgpt-web/browserSession.js` | 21 | 15% |
| `open-sse/executors/chatgpt-web/chatHttp.js` | 19 | 12% |
| `open-sse/executors/chatgpt-web/deltaV1.js` | 20 | 16% |
| `open-sse/executors/chatgpt-web/firstParty.js` | 79 | 6% |
| `open-sse/executors/chatgpt-web/imageGen.js` | 36 | 14% |
| `open-sse/executors/chatgpt-web/imageGenHttp.js` | 29 | 13% |
| `open-sse/executors/chatgpt-web/imageSniff.js` | 6 | 0% |
| `open-sse/executors/chatgpt-web/sentinel.js` | 11 | 0% |
| `open-sse/executors/codebuddy-cn.js` | 7 | 20% |
| `open-sse/executors/codebuddy-intl.js` | 4 | 0% |
| `open-sse/executors/codex.js` | 43 | 18% |
| `open-sse/executors/commandcode.js` | 16 | 29% |
| `open-sse/executors/cursor.js` | 92 | 15% |
| `open-sse/executors/deepseek-web-done-terminator.js` | 8 | 0% |
| `open-sse/executors/deepseek-web/pow.js` | 2 | 0% |
| `open-sse/executors/deepseek-web/stream-format.js` | 8 | 0% |
| `open-sse/executors/gemini-web/browserLease.js` | 0 | 0% |
| `open-sse/executors/gemini-web/domCapture.js` | 0 | 0% |
| `open-sse/executors/gemini-web/modeSelection.js` | 0 | 0% |
| `open-sse/executors/grok-web-tools.js` | 0 | 0% |
| `open-sse/executors/muse-web/dom.js` | 0 | 0% |
| `open-sse/executors/zed.js` | 0 | 0% |
| `open-sse/lib/deepseek-pow-hash.js` | 0 | 0% |
| `open-sse/lib/deepseek-pow-worker.mjs` | 0 | 0% |
| `open-sse/lib/deepseek-pow.js` | 0 | 0% |
| `open-sse/services/grokTlsClient.js` | 0 | 0% |
| `open-sse/services/opencodeTlsClient.js` | 0 | 0% |
| `open-sse/shared/awsCredentials.js` | 0 | 0% |
| `open-sse/shared/clineAuth.js` | 0 | 0% |
| `open-sse/shared/clineEnvelope.js` | 0 | 0% |
| `open-sse/shared/commandCodeHeaders.js` | 0 | 0% |
| `open-sse/shared/mimoAccount.js` | 0 | 0% |
| `open-sse/shared/zedAuth.js` | 0 | 0% |
| `open-sse/utils/awsSigv4.js` | 0 | 0% |
| `open-sse/utils/claudeCloaking.js` | 0 | 0% |
| `open-sse/utils/codexToolSchema.js` | 0 | 0% |
| `open-sse/utils/cursorChecksum.js` | 0 | 0% |
| `open-sse/utils/cursorProtobuf.js` | 0 | 0% |
| `open-sse/utils/kiroSessionReplay.js` | 0 | 0% |
| `open-sse/utils/opencodeFingerprint.js` | 0 | 0% |
| `open-sse/utils/opencodeResetTime.js` | 0 | 0% |
| `src/shared/utils/clineAuth.js` | 0 | 0% |

## Streaming / transform utils (16)

| JS module | functions | name coverage |
|---|---|---|
| `open-sse/shared/qoder/sse.js` | 0 | 0% |
| `open-sse/transformer/responsesTransformer.js` | 0 | 0% |
| `open-sse/transformer/streamToJsonConverter.js` | 0 | 0% |
| `open-sse/utils/bypassHandler.js` | 0 | 0% |
| `open-sse/utils/debugLog.js` | 0 | 0% |
| `open-sse/utils/proxyFetch.js` | 0 | 0% |
| `open-sse/utils/reasoningContentInjector.js` | 0 | 0% |
| `open-sse/utils/requestLogger.js` | 0 | 0% |
| `open-sse/utils/sessionManager.js` | 0 | 0% |
| `open-sse/utils/sse.js` | 0 | 0% |
| `open-sse/utils/sseConstants.js` | 0 | 0% |
| `open-sse/utils/stableJson.js` | 0 | 0% |
| `open-sse/utils/streamHandler.js` | 0 | 0% |
| `open-sse/utils/usageTracking.js` | 0 | 0% |
| `open-sse/utils/writableDataDir.js` | 0 | 0% |
| `src/lib/providerNormalization.js` | 0 | 0% |

## Token saver (rtk) (10)

| JS module | functions | name coverage |
|---|---|---|
| `open-sse/rtk/applyFilter.js` | 0 | 0% |
| `open-sse/rtk/autodetect.js` | 0 | 0% |
| `open-sse/rtk/caveman.js` | 0 | 0% |
| `open-sse/rtk/cavemanPrompts.js` | 0 | 0% |
| `open-sse/rtk/filters/buildOutput.js` | 0 | 0% |
| `open-sse/rtk/filters/gitLog.js` | 0 | 0% |
| `open-sse/rtk/filters/ls.js` | 0 | 0% |
| `open-sse/rtk/index.js` | 0 | 0% |
| `open-sse/rtk/ponytailPrompt.js` | 0 | 0% |
| `open-sse/rtk/systemInject.js` | 0 | 0% |

## Config / catalogs (16)

| JS module | functions | name coverage |
|---|---|---|
| `open-sse/config/appConstants.js` | 9 | 0% |
| `open-sse/config/awsConstants.js` | 0 | 0% |
| `open-sse/config/defaultThinkingSignature.js` | 0 | 0% |
| `open-sse/config/errorConfig.js` | 0 | 0% |
| `open-sse/config/grokCli.js` | 1 | 0% |
| `open-sse/config/kiroConstants.js` | 24 | 4% |
| `open-sse/config/models.js` | 1 | 0% |
| `open-sse/config/ollamaModels.js` | 0 | 0% |
| `open-sse/config/providerModels.js` | 19 | 0% |
| `open-sse/config/providers.js` | 2 | 0% |
| `open-sse/config/runtimeConfig.js` | 4 | 0% |
| `open-sse/providers/catalogOverride.js` | 0 | 0% |
| `open-sse/providers/visionPatterns.js` | 0 | 0% |
| `src/lib/compatibleModels.js` | 0 | 0% |
| `src/lib/grokBuildConfig.js` | 0 | 0% |
| `src/lib/userModelCaps.js` | 0 | 0% |

## Persistence & ops (db, logs, updater, eval) (45)

| JS module | functions | name coverage |
|---|---|---|
| `src/lib/appUpdater.js` | 0 | 0% |
| `src/lib/atRestCrypto.js` | 0 | 0% |
| `src/lib/batchTestRegistry.js` | 0 | 0% |
| `src/lib/consoleLogBuffer.js` | 0 | 0% |
| `src/lib/db/adapters/betterSqliteAdapter.js` | 0 | 0% |
| `src/lib/db/adapters/bunSqliteAdapter.js` | 0 | 0% |
| `src/lib/db/adapters/nodeSqliteAdapter.js` | 0 | 0% |
| `src/lib/db/adapters/sqljsAdapter.js` | 0 | 0% |
| `src/lib/db/driver.js` | 0 | 0% |
| `src/lib/db/helpers/jsonCol.js` | 0 | 0% |
| `src/lib/db/helpers/kvStore.js` | 0 | 0% |
| `src/lib/db/helpers/metaStore.js` | 0 | 0% |
| `src/lib/db/helpers/nullBytes.js` | 0 | 0% |
| `src/lib/db/index.js` | 0 | 0% |
| `src/lib/db/migrations/001-initial.js` | 0 | 0% |
| `src/lib/db/normalizeInjections.js` | 0 | 0% |
| `src/lib/db/repos/aliasRepo.js` | 0 | 0% |
| `src/lib/db/repos/apiKeysRepo.js` | 0 | 0% |
| `src/lib/db/repos/briefModeRepo.js` | 0 | 0% |
| `src/lib/db/repos/combosRepo.js` | 0 | 0% |
| `src/lib/db/repos/connectionsRepo.js` | 0 | 0% |
| `src/lib/db/repos/disabledModelsRepo.js` | 0 | 0% |
| `src/lib/db/repos/keyLimitRepo.js` | 0 | 0% |
| `src/lib/db/repos/keyQuotaRepo.js` | 0 | 0% |
| `src/lib/db/repos/nodesRepo.js` | 0 | 0% |
| `src/lib/db/repos/pricingRepo.js` | 0 | 0% |
| `src/lib/db/repos/promptInjectRepo.js` | 0 | 0% |
| `src/lib/db/repos/promptInjectStatsRepo.js` | 0 | 0% |
| `src/lib/db/repos/proxyPoolsRepo.js` | 0 | 0% |
| `src/lib/db/repos/requestDetailsRepo.js` | 0 | 0% |
| `src/lib/db/repos/settingsRepo.js` | 0 | 0% |
| `src/lib/db/repos/tokenSaverStatsRepo.js` | 0 | 0% |
| `src/lib/db/repos/usageRepo.js` | 0 | 0% |
| `src/lib/disabledModelsDb.js` | 0 | 0% |
| `src/lib/evalRunner.js` | 0 | 0% |
| `src/lib/localDb.js` | 0 | 0% |
| `src/lib/logLineParse.js` | 0 | 0% |
| `src/lib/mimoLoginSession.js` | 0 | 0% |
| `src/lib/mitmAliasCache.js` | 0 | 0% |
| `src/lib/providerConnectionResponse.js` | 0 | 0% |
| `src/lib/providerHealthSummary.js` | 0 | 0% |
| `src/lib/providerStatsCache.js` | 0 | 0% |
| `src/lib/requestDetailsDb.js` | 0 | 0% |
| `src/lib/updater/updater.js` | 0 | 0% |
| `src/lib/usageDb.js` | 0 | 0% |

## Credentials / OAuth runtime (12)

| JS module | functions | name coverage |
|---|---|---|
| `open-sse/services/browserProfileCache.js` | 0 | 0% |
| `open-sse/services/lazyRuntime.js` | 0 | 0% |
| `open-sse/services/oauthCredentialManager.js` | 0 | 0% |
| `open-sse/services/thoughtSignatureStore.js` | 0 | 0% |
| `open-sse/services/tokenRefresh.js` | 0 | 0% |
| `open-sse/services/webLoginConfig.js` | 0 | 0% |
| `src/lib/auth/dashboardSession.js` | 0 | 0% |
| `src/lib/auth/dashboardThrottle.js` | 0 | 0% |
| `src/lib/auth/loginLimiter.js` | 0 | 0% |
| `src/lib/auth/ssoTestGate.js` | 0 | 0% |
| `src/lib/auth/trustedPeer.js` | 0 | 0% |
| `src/sse/services/tokenRefresh.js` | 0 | 0% |

## MITM / tunnel / network (19)

| JS module | functions | name coverage |
|---|---|---|
| `src/dashboardGuard.js` | 0 | 0% |
| `src/lib/network/initOutboundProxy.js` | 0 | 0% |
| `src/lib/network/netlifyRelay.js` | 0 | 0% |
| `src/lib/network/proxyHealthJob.js` | 0 | 0% |
| `src/lib/network/proxyTest.js` | 0 | 0% |
| `src/lib/tunnel/cloudflare/healthCheck.js` | 0 | 0% |
| `src/lib/tunnel/cloudflare/manager.js` | 0 | 0% |
| `src/lib/tunnel/cloudflare/pid.js` | 0 | 0% |
| `src/lib/tunnel/shared/dnsResolver.js` | 0 | 0% |
| `src/lib/tunnel/shared/internetCheck.js` | 0 | 0% |
| `src/lib/tunnel/shared/watchdogConfig.js` | 0 | 0% |
| `src/lib/tunnel/tailscale/healthCheck.js` | 0 | 0% |
| `src/lib/tunnel/tailscale/manager.js` | 0 | 0% |
| `src/mitm/cert/rootCA.js` | 0 | 0% |
| `src/mitm/dbReader.js` | 0 | 0% |
| `src/mitm/dns/dnsConfig.js` | 0 | 0% |
| `src/mitm/logger.js` | 0 | 0% |
| `src/mitm/manager.js` | 0 | 0% |
| `src/mitm/winElevated.js` | 0 | 0% |

## Other (94)

| JS module | functions | name coverage |
|---|---|---|
| `open-sse/handlers/chatCore/nonStreamingHandler.js` | 0 | 0% |
| `open-sse/handlers/chatCore/sseToJsonHandler.js` | 0 | 0% |
| `open-sse/handlers/chatCore/streamingHandler.js` | 0 | 0% |
| `open-sse/handlers/embeddingProviders/index.js` | 0 | 0% |
| `open-sse/handlers/embeddingProviders/openaiCompatNode.js` | 0 | 0% |
| `open-sse/handlers/embeddingProviders/selfhostedEmbedding.js` | 0 | 0% |
| `open-sse/handlers/imageProviders/blackForestLabs.js` | 0 | 0% |
| `open-sse/handlers/imageProviders/cloudflareAi.js` | 0 | 0% |
| `open-sse/handlers/imageProviders/falAi.js` | 0 | 0% |
| `open-sse/handlers/imageProviders/index.js` | 0 | 0% |
| `open-sse/handlers/search/chatSearch.js` | 0 | 0% |
| `open-sse/handlers/search/normalizers.js` | 0 | 0% |
| `open-sse/providers/models/namePatterns.js` | 0 | 0% |
| `open-sse/providers/registry/bai.js` | 0 | 0% |
| `open-sse/providers/registry/exa.js` | 0 | 0% |
| `open-sse/providers/registry/glm.js` | 0 | 0% |
| `open-sse/providers/registry/mmf.js` | 0 | 0% |
| `open-sse/providers/registry/v1m.js` | 0 | 0% |
| `open-sse/providers/registry/xai.js` | 0 | 0% |
| `open-sse/providers/registry/zed.js` | 0 | 0% |
| `open-sse/services/usage/antigravity-weekly.js` | 0 | 0% |
| `open-sse/services/usage/glm.js` | 0 | 0% |
| `open-sse/services/usage/grokCliQuotaFrame.js` | 0 | 0% |
| `open-sse/services/usage/zed.js` | 0 | 0% |
| `open-sse/shared/qoder/attachments.js` | 0 | 0% |
| `open-sse/shared/qoder/contextTier.js` | 0 | 0% |
| `open-sse/shared/qoder/cosy.js` | 0 | 0% |
| `open-sse/shared/qoder/queue.js` | 0 | 0% |
| `open-sse/translator/concerns/paramSupport.js` | 0 | 0% |
| `open-sse/translator/concerns/prefetch.js` | 0 | 0% |
| `open-sse/translator/concerns/thinkingUnified.js` | 0 | 0% |
| `open-sse/translator/deepseekWebToolBinding.js` | 0 | 0% |
| `open-sse/translator/schema/finishReasons.js` | 0 | 0% |
| `open-sse/translator/schema/roles.js` | 0 | 0% |
| `open-sse/translator/schema/streamEvents.js` | 0 | 0% |
| `src/app/api/auth/saml/acs/route.js` | 0 | 0% |
| `src/app/api/cli-tools/hermes-settings/hermesYaml.js` | 0 | 0% |
| `src/app/api/cli-tools/resolveApiKey.js` | 0 | 0% |
| `src/app/api/headroom/proxy/[...path]/route.js` | 0 | 0% |
| `src/app/api/mcp/[plugin]/sse/route.js` | 0 | 0% |
| `src/app/api/oauth/[provider]/[action]/route.js` | 0 | 0% |
| `src/app/api/oauth/gitlab/pat/route.js` | 0 | 0% |
| `src/app/api/providers/[id]/test/testUtils.js` | 0 | 0% |
| `src/app/api/usage/[connectionId]/route.js` | 0 | 0% |
| `src/app/api/v1/route.js` | 0 | 0% |
| `src/app/api/v1beta/models/[...path]/route.js` | 0 | 0% |
| `src/lib/backup/autoBackup.js` | 0 | 0% |
| `src/lib/oauth/constants/xai.js` | 0 | 0% |
| `src/lib/oauth/kiroExternalIdp.js` | 0 | 0% |
| `src/lib/oauth/providerHelpers.js` | 0 | 0% |
| `src/lib/oauth/providers/glm.js` | 0 | 0% |
| `src/lib/oauth/providers/minimax-code-shared.js` | 0 | 0% |
| `src/lib/oauth/providers/xai.js` | 0 | 0% |
| `src/lib/oauth/providers/zed.js` | 0 | 0% |
| `src/lib/oauth/services/xai.js` | 0 | 0% |
| `src/lib/oauth/utils/ideDetect.js` | 0 | 0% |
| `src/lib/oauth/utils/ui.js` | 0 | 0% |
| `src/lib/oauth/utils/zedCredentials.js` | 0 | 0% |
| `src/lib/pxpipe/loader.js` | 0 | 0% |
| `src/lib/qoder/cosy.js` | 0 | 0% |
| `src/shared/constants/accents.js` | 0 | 0% |
| `src/shared/constants/builtinPromptTemplates.js` | 0 | 0% |
| `src/shared/constants/colors.js` | 0 | 0% |
| `src/shared/constants/coworkPlugins.js` | 0 | 0% |
| `src/shared/constants/mitmToolHosts.js` | 0 | 0% |
| `src/shared/constants/nameStyles.js` | 0 | 0% |
| `src/shared/constants/providersDisplay.js` | 0 | 0% |
| `src/shared/constants/ttsProviders.js` | 0 | 0% |
| `src/shared/services/initializeApp.js` | 0 | 0% |
| `src/shared/services/quotaAutoPing.js` | 0 | 0% |
| `src/shared/services/quotaPingSchedule.js` | 0 | 0% |
| `src/shared/utils/api.js` | 0 | 0% |
| `src/shared/utils/bulkAdd.js` | 0 | 0% |
| `src/shared/utils/cn.js` | 0 | 0% |
| `src/shared/utils/connectionStatus.js` | 0 | 0% |
| `src/shared/utils/errorText.js` | 0 | 0% |
| `src/shared/utils/friendlyBacking.js` | 0 | 0% |
| `src/shared/utils/modelSearchFilter.js` | 0 | 0% |
| `src/shared/utils/providerCustomModels.js` | 0 | 0% |
| `src/shared/utils/providerIcon.js` | 0 | 0% |
| `src/shared/utils/providerModelsFetcher.js` | 0 | 0% |
| `src/shared/utils/ssrfGuard.js` | 0 | 0% |
| `src/shared/utils/testErrorStage.js` | 0 | 0% |
| `src/sse/services/antigravityQuota.js` | 0 | 0% |
| `src/sse/services/backgroundTokenRefresh.js` | 0 | 0% |
| `src/sse/services/capacityOrder.js` | 0 | 0% |
| `src/sse/services/comboDispatch.js` | 0 | 0% |
| `src/sse/services/keyAdmission.js` | 0 | 0% |
| `src/sse/services/memberFailover.js` | 0 | 0% |
| `src/sse/services/modalityUsage.js` | 0 | 0% |
| `src/sse/services/modelsListCache.js` | 0 | 0% |
| `src/sse/services/pairStrategy.js` | 0 | 0% |
| `src/sse/services/providerBreaker.js` | 0 | 0% |
| `src/sse/utils/logger.js` | 0 | 0% |
