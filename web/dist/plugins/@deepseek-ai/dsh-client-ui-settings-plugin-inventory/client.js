window.__ModuleLoader__.load({
	id: "@deepseek-ai/dsh-client-ui-settings-plugin-inventory",
	factory: (require) => {
		var module = { exports: {} };
		var exports = module.exports;
		Object.defineProperty(exports, Symbol.toStringTag, { value: "Module" });
		let react_jsx_runtime = require("react/jsx-runtime");
		let react = require("react");
		let _deepseek_ai_dsh_client_ui_primitives = require("@deepseek-ai/dsh-client-ui-primitives");
		//#region \0dsh-css:C:\Users\24763\AppData\Local\Temp\yunxi-next-dsh-1b724c4edaa94137b565704a3acef4c3\packages\client\ui-settings-plugin-inventory\src\client\PluginInventorySettingsTab.module.css.mjs
		const css = ".t29tjW_section{width:100%;max-width:760px;color:var(--dsw-alias-label-primary);flex-direction:column;gap:14px;display:flex}.t29tjW_catalogHeading h3,.t29tjW_status,.t29tjW_failure p{margin:0}.t29tjW_status,.t29tjW_failure{color:var(--dsw-alias-label-tertiary);font-size:13px;line-height:20px}.t29tjW_failure{color:var(--dsw-alias-state-error-primary);align-items:center;gap:10px;display:flex}.t29tjW_failure button{border:1px solid var(--dsw-alias-border-l2);color:var(--dsw-alias-label-primary);font:inherit;cursor:pointer;background:0 0;border-radius:6px;padding:4px 10px}.t29tjW_catalog{flex-direction:column;gap:12px;display:flex}.t29tjW_search{width:100%;color:var(--dsw-alias-label-tertiary);align-items:center;display:flex;position:relative}.t29tjW_search>svg{pointer-events:none;position:absolute;left:12px}.t29tjW_search input{border:1px solid var(--dsw-alias-border-l2);background:var(--dsw-alias-bg-layer-1);width:100%;height:36px;color:var(--dsw-alias-label-primary);font:inherit;border-radius:8px;outline:none;padding:0 34px 0 36px;font-size:13px}.t29tjW_search input::placeholder{color:var(--dsw-alias-label-tertiary)}.t29tjW_search input:focus-visible{border-color:var(--dsw-alias-state-business-primary);box-shadow:0 0 0 2px color-mix(in srgb, var(--dsw-alias-state-business-primary) 18%, transparent)}.t29tjW_catalogHeading{align-items:baseline;gap:7px;padding:0 2px;display:flex}.t29tjW_catalogHeading h3{font-size:13px;font-weight:600;line-height:20px}.t29tjW_catalogHeading span{color:var(--dsw-alias-label-tertiary);font-variant-numeric:tabular-nums;font-size:12px;line-height:18px}.t29tjW_cards{grid-template-columns:repeat(2,minmax(0,1fr));align-items:start;gap:10px;margin:0;padding:0;list-style:none;display:grid}.t29tjW_card{border:1px solid var(--dsw-alias-border-l2);background:var(--dsw-alias-bg-layer-3);border-radius:8px;min-width:0;overflow:hidden}.t29tjW_card[data-open=true]{border-color:var(--dsw-alias-border-l1);box-shadow:var(--dsw-shadow-lv1)}.t29tjW_cardHeader{align-items:center;min-width:0;display:flex}.t29tjW_cardContent{box-sizing:border-box;min-width:0;min-height:52px;color:inherit;font:inherit;text-align:left;cursor:pointer;background:0 0;border:0;flex:auto;justify-content:space-between;align-items:center;gap:10px;padding:12px 8px 12px 14px;display:flex}.t29tjW_cardContent:hover,.t29tjW_card[data-open=true] .t29tjW_cardContent{background:var(--dsw-alias-interactive-bg-hover)}.t29tjW_cardContent:focus-visible{outline:2px solid var(--dsw-alias-state-business-primary);outline-offset:-2px}.t29tjW_cardTitle{text-overflow:ellipsis;white-space:nowrap;min-width:0;font-size:14px;font-weight:600;line-height:20px;overflow:hidden}.t29tjW_cardTrailing{color:var(--dsw-alias-label-tertiary);flex:none;align-items:center;gap:6px;display:inline-flex}.t29tjW_statusDot{background:var(--dsw-alias-label-tertiary);border-radius:999px;flex:none;width:7px;height:7px;display:inline-block}.t29tjW_statusDot[data-phase=active]{background:var(--dsw-alias-state-success-primary)}.t29tjW_statusDot[data-phase=failed]{background:var(--dsw-alias-state-error-primary)}.t29tjW_statusDot[data-phase=loading]{background:var(--dsw-alias-state-business-primary)}.t29tjW_configTag{background:var(--dsw-alias-bg-layer-1);min-height:20px;color:var(--dsw-alias-label-secondary);white-space:nowrap;border-radius:5px;align-items:center;padding:1px 6px;font-size:11px;line-height:16px;display:inline-flex}.t29tjW_configTag[data-enabled=true]{background:color-mix(in srgb, var(--dsw-alias-state-success-primary) 10%, transparent);color:var(--dsw-alias-state-success-primary)}.t29tjW_configTag[data-pending=true]{background:color-mix(in srgb, var(--dsw-alias-state-warn-label) 12%, transparent);color:var(--dsw-alias-state-warn-label)}.t29tjW_chevron{color:var(--dsw-alias-label-tertiary);flex:none}.t29tjW_card[data-open=true] .t29tjW_chevron{transform:rotate(180deg)}.t29tjW_capabilitySwitch{background:var(--dsw-alias-fill-l2);cursor:pointer;border:0;border-radius:999px;flex:0 0 34px;width:34px;height:20px;margin:0 12px 0 2px;padding:0;position:relative}.t29tjW_capabilitySwitch>span{background:var(--dsw-alias-bg-layer-3);width:14px;height:14px;box-shadow:var(--dsw-shadow-lv1);border-radius:50%;position:absolute;top:3px;left:3px}.t29tjW_capabilitySwitch[data-checked=true]{background:var(--dsw-alias-state-business-primary)}.t29tjW_capabilitySwitch[data-checked=true]>span{transform:translate(14px)}.t29tjW_capabilitySwitch:focus-visible{outline:2px solid var(--dsw-alias-state-business-primary);outline-offset:2px}.t29tjW_capabilitySwitch:disabled{cursor:not-allowed;opacity:.48}.t29tjW_cardDetails{border-top:1px solid var(--dsw-alias-border-l2);background:var(--dsw-alias-bg-module-platform);padding:10px 14px 12px}.t29tjW_entryValue{overflow-wrap:anywhere;color:var(--dsw-alias-label-primary);font-family:var(--ds-font-family-code);font-size:12px;line-height:18px;display:block}.t29tjW_details{grid-template-columns:76px minmax(0,1fr);gap:6px 10px;margin:8px 0 0;display:grid}.t29tjW_details div{display:contents}.t29tjW_details dt{color:var(--dsw-alias-label-tertiary);font-size:11px;line-height:17px}.t29tjW_details dd{overflow-wrap:anywhere;min-width:0;color:var(--dsw-alias-label-secondary);margin:0;font-size:12px;line-height:17px}.t29tjW_visuallyHidden{clip:rect(0 0 0 0);clip-path:inset(50%);white-space:nowrap;width:1px;height:1px;position:absolute;overflow:hidden}@media (prefers-reduced-motion:no-preference){.t29tjW_chevron,.t29tjW_capabilitySwitch,.t29tjW_capabilitySwitch>span{transition:transform .14s var(--ds-ease-in-out), background-color .14s var(--ds-ease-in-out)}}@media (width<=680px){[role=dialog]:has(.t29tjW_section){flex-direction:column}[role=dialog]:has(.t29tjW_section)>nav{box-sizing:border-box;flex:none;gap:8px;width:100%;height:auto;padding:16px 48px 8px 12px}[role=dialog]:has(.t29tjW_section)>nav>div:first-child{width:auto}[role=dialog]:has(.t29tjW_section)>nav>div:last-child{grid-template-columns:repeat(2,minmax(0,1fr));gap:6px 10px;width:100%;height:auto;display:grid}[role=dialog]:has(.t29tjW_section)>nav>div:last-child>button{gap:6px;width:100%;padding-left:8px;padding-right:8px}[role=dialog]:has(.t29tjW_section)>nav>div:last-child>button>span{width:auto;min-width:0}[role=dialog]:has(.t29tjW_section)>nav+div{width:100%;min-width:0;min-height:0}[role=dialog]:has(.t29tjW_section)>nav+div>div:first-child{z-index:1;width:auto;height:auto;padding:0;position:absolute;top:12px;right:12px}[role=dialog]:has(.t29tjW_section)>nav+div>div:last-child{box-sizing:border-box;width:100%;padding:0 16px 16px}.t29tjW_cards{grid-template-columns:minmax(0,1fr)}}";
		const tagId = "@deepseek-ai/dsh-client-ui-settings-plugin-inventory/PluginInventorySettingsTab.module.css";
		if (typeof document !== "undefined" && document.querySelector("style[data-plugin-css=" + JSON.stringify(tagId) + "]") === null) {
			const tag = document.createElement("style");
			tag.dataset.plugin = "@deepseek-ai/dsh-client-ui-settings-plugin-inventory";
			tag.dataset.pluginCss = tagId;
			tag.textContent = css;
			document.head.appendChild(tag);
		}
		var PluginInventorySettingsTab_module_css_default = {
			"capabilitySwitch": "t29tjW_capabilitySwitch",
			"card": "t29tjW_card",
			"cardContent": "t29tjW_cardContent",
			"cardDetails": "t29tjW_cardDetails",
			"cardHeader": "t29tjW_cardHeader",
			"cardTitle": "t29tjW_cardTitle",
			"cardTrailing": "t29tjW_cardTrailing",
			"cards": "t29tjW_cards",
			"catalog": "t29tjW_catalog",
			"catalogHeading": "t29tjW_catalogHeading",
			"chevron": "t29tjW_chevron",
			"configTag": "t29tjW_configTag",
			"details": "t29tjW_details",
			"entryValue": "t29tjW_entryValue",
			"failure": "t29tjW_failure",
			"search": "t29tjW_search",
			"section": "t29tjW_section",
			"status": "t29tjW_status",
			"statusDot": "t29tjW_statusDot",
			"visuallyHidden": "t29tjW_visuallyHidden"
		};
		//#endregion
		//#region lib/types/client/PluginInventorySettingsTab.js
		const PHASE_KEYS = {
			pending: "pending",
			loading: "loadingPhase",
			active: "active",
			failed: "failed",
			unloading: "unloading"
		};
		const CAPABILITY_BY_ENTRY = {
			"yunxi.context": "context",
			"yunxi.persona": "persona",
			"yunxi.memory": "memory",
			"yunxi.companion": "companion",
			"yunxi.storage": "storage",
			"yunxi.companion-mailbox": "mailbox",
			"yunxi.scheduler": "scheduler",
			"yunxi.tool.shell": "shell",
			"yunxi.tool.patch": "patch",
			"yunxi.tool.files": "files",
			"yunxi.tool.mcp": "mcp",
			"yunxi.tool.skills": "skills"
		};
		function phaseLabel(phase, t) {
			return phase === null ? t("unobserved") : t(PHASE_KEYS[phase]);
		}
		function moduleShortName(moduleName) {
			return (moduleName.startsWith("@") ? moduleName.slice(moduleName.indexOf("/") + 1) : moduleName).replace(/^cordis:/, "").replace(/^cordis-plugin-/, "").replace(/^dsh-(?:host-|client-)?/, "").replace(/^yunxi\.plugin\.yunxi\./, "");
		}
		function matches(entry, normalizedQuery) {
			if (normalizedQuery.length === 0) return true;
			return [entry.moduleName, entry.entryId].some((value) => value.toLocaleLowerCase().includes(normalizedQuery));
		}
		/** Render the current inventory with restart-scoped YunXi capability switches. */
		function PluginInventorySettingsTab({ list, getCapabilities, subscribeCapabilities, setCapability, t }) {
			const catalogId = (0, react.useId)();
			const [request, setRequest] = (0, react.useState)(0);
			const [query, setQuery] = (0, react.useState)("");
			const [expanded, setExpanded] = (0, react.useState)(null);
			const [state, setState] = (0, react.useState)({ status: "loading" });
			const [savingFields, setSavingFields] = (0, react.useState)(() => /* @__PURE__ */ new Set());
			const capabilities = (0, react.useSyncExternalStore)(subscribeCapabilities, getCapabilities, getCapabilities);
			(0, react.useEffect)(() => {
				let current = true;
				Promise.resolve().then(() => list()).then((snapshot) => {
					if (current) setState({
						status: "ready",
						snapshot
					});
				}, () => {
					if (current) setState({ status: "error" });
				});
				return () => {
					current = false;
				};
			}, [list, request]);
			const normalizedQuery = query.trim().toLocaleLowerCase();
			const filteredEntries = (0, react.useMemo)(() => state.status === "ready" ? state.snapshot.entries.filter((entry) => matches(entry, normalizedQuery)) : [], [normalizedQuery, state]);
			(0, react.useEffect)(() => {
				if (expanded !== null && !filteredEntries.some((entry) => entry.entryId === expanded)) setExpanded(null);
			}, [expanded, filteredEntries]);
			const retry = () => {
				setState({ status: "loading" });
				setRequest((value) => value + 1);
			};
			const chooseCapability = (field, enabled) => {
				setSavingFields((previous) => new Set([...previous, field]));
				setCapability(field, enabled).finally(() => {
					setSavingFields((previous) => {
						const next = new Set(previous);
						next.delete(field);
						return next;
					});
				});
			};
			return (0, react_jsx_runtime.jsxs)("div", {
				className: PluginInventorySettingsTab_module_css_default.section,
				"aria-busy": state.status === "loading",
				children: [
					state.status === "loading" ? (0, react_jsx_runtime.jsx)("p", {
						className: PluginInventorySettingsTab_module_css_default.status,
						children: t("loading")
					}) : null,
					state.status === "error" ? (0, react_jsx_runtime.jsxs)("div", {
						className: PluginInventorySettingsTab_module_css_default.failure,
						children: [(0, react_jsx_runtime.jsx)("p", {
							role: "alert",
							children: t("error")
						}), (0, react_jsx_runtime.jsx)("button", {
							type: "button",
							onClick: retry,
							children: t("retry")
						})]
					}) : null,
					state.status === "ready" ? (0, react_jsx_runtime.jsxs)("div", {
						className: PluginInventorySettingsTab_module_css_default.catalog,
						children: [
							(0, react_jsx_runtime.jsxs)("label", {
								className: PluginInventorySettingsTab_module_css_default.search,
								children: [
									(0, react_jsx_runtime.jsx)(_deepseek_ai_dsh_client_ui_primitives.IconSearchOutline16, { "aria-hidden": "true" }),
									(0, react_jsx_runtime.jsx)("span", {
										className: PluginInventorySettingsTab_module_css_default.visuallyHidden,
										children: t("search")
									}),
									(0, react_jsx_runtime.jsx)("input", {
										type: "search",
										value: query,
										placeholder: t("search"),
										"aria-label": t("search"),
										onChange: (event) => {
											setQuery(event.currentTarget.value);
										}
									})
								]
							}),
							(0, react_jsx_runtime.jsxs)("div", {
								className: PluginInventorySettingsTab_module_css_default.catalogHeading,
								children: [(0, react_jsx_runtime.jsx)("h3", { children: t("catalog") }), (0, react_jsx_runtime.jsx)("span", {
									"data-plugin-count": filteredEntries.length,
									children: filteredEntries.length
								})]
							}),
							state.snapshot.entries.length === 0 ? (0, react_jsx_runtime.jsx)("p", {
								className: PluginInventorySettingsTab_module_css_default.status,
								children: t("empty")
							}) : null,
							state.snapshot.entries.length > 0 && filteredEntries.length === 0 ? (0, react_jsx_runtime.jsx)("p", {
								className: PluginInventorySettingsTab_module_css_default.status,
								children: t("emptySearch")
							}) : null,
							filteredEntries.length > 0 ? (0, react_jsx_runtime.jsx)("ul", {
								className: PluginInventorySettingsTab_module_css_default.cards,
								children: filteredEntries.map((entry) => {
									const status = phaseLabel(entry.fiberPhase, t);
									const title = moduleShortName(entry.moduleName);
									const field = CAPABILITY_BY_ENTRY[String(entry.entryId)];
									const configured = field === void 0 ? entry.enabled : capabilities.value?.[field] ?? entry.enabled;
									const restartPending = field !== void 0 && configured !== entry.enabled;
									const configuration = field === void 0 ? t("coreTag") : restartPending ? t("restartPendingTag") : t(configured ? "enabledTag" : "disabledTag");
									const runtime = t(entry.enabled ? "enabledTag" : "disabledTag");
									const open = expanded === entry.entryId;
									const detailId = `${catalogId}-details-${encodeURIComponent(entry.entryId)}`;
									const saving = field !== void 0 && savingFields.has(field);
									const canWrite = field !== void 0 && capabilities.status === "ready" && capabilities.writable && !saving;
									const toggleLabel = field === void 0 ? t("unavailable") : t(configured ? "disable" : "enable");
									return (0, react_jsx_runtime.jsxs)("li", {
										className: PluginInventorySettingsTab_module_css_default.card,
										"data-plugin-entry": entry.entryId,
										"data-open": open ? "true" : void 0,
										"data-pending-restart": restartPending ? "true" : void 0,
										children: [(0, react_jsx_runtime.jsxs)("div", {
											className: PluginInventorySettingsTab_module_css_default.cardHeader,
											children: [(0, react_jsx_runtime.jsxs)("button", {
												className: PluginInventorySettingsTab_module_css_default.cardContent,
												type: "button",
												"aria-expanded": open,
												"aria-controls": detailId,
												"aria-label": `${title}, ${status}, ${configuration}`,
												onClick: () => {
													setExpanded((current) => current === entry.entryId ? null : entry.entryId);
												},
												children: [(0, react_jsx_runtime.jsx)("strong", {
													className: PluginInventorySettingsTab_module_css_default.cardTitle,
													title: entry.moduleName,
													children: title
												}), (0, react_jsx_runtime.jsxs)("span", {
													className: PluginInventorySettingsTab_module_css_default.cardTrailing,
													children: [
														entry.enabled ? (0, react_jsx_runtime.jsx)("span", {
															className: PluginInventorySettingsTab_module_css_default.statusDot,
															"data-phase": entry.fiberPhase ?? "unobserved",
															role: "img",
															"aria-label": status,
															title: status
														}) : null,
														(0, react_jsx_runtime.jsx)("span", {
															className: PluginInventorySettingsTab_module_css_default.configTag,
															"data-enabled": configured ? "true" : "false",
															"data-pending": restartPending ? "true" : void 0,
															children: configuration
														}),
														(0, react_jsx_runtime.jsx)(_deepseek_ai_dsh_client_ui_primitives.IconChevronDownOutline14, {
															className: PluginInventorySettingsTab_module_css_default.chevron,
															size: 12,
															"aria-hidden": "true"
														})
													]
												})]
											}), field !== void 0 ? (0, react_jsx_runtime.jsx)("button", {
												className: PluginInventorySettingsTab_module_css_default.capabilitySwitch,
												type: "button",
												role: "switch",
												"aria-checked": configured,
												"aria-label": `${toggleLabel} ${title}`,
												title: `${toggleLabel} ${title}`,
												"data-capability-field": field,
												"data-checked": configured ? "true" : "false",
												disabled: !canWrite,
												onClick: () => {
													chooseCapability(field, !configured);
												},
												children: (0, react_jsx_runtime.jsx)("span", { "aria-hidden": "true" })
											}) : null]
										}), open ? (0, react_jsx_runtime.jsxs)("div", {
											className: PluginInventorySettingsTab_module_css_default.cardDetails,
											id: detailId,
											children: [(0, react_jsx_runtime.jsx)("code", {
												className: PluginInventorySettingsTab_module_css_default.entryValue,
												"data-loader-entry": true,
												children: entry.entryId
											}), (0, react_jsx_runtime.jsxs)("dl", {
												className: PluginInventorySettingsTab_module_css_default.details,
												children: [
													(0, react_jsx_runtime.jsxs)("div", { children: [(0, react_jsx_runtime.jsx)("dt", { children: t("configuration") }), (0, react_jsx_runtime.jsx)("dd", { children: field === void 0 ? t("coreTag") : t(configured ? "enabledTag" : "disabledTag") })] }),
													(0, react_jsx_runtime.jsxs)("div", { children: [(0, react_jsx_runtime.jsx)("dt", { children: t("runtime") }), (0, react_jsx_runtime.jsx)("dd", { children: runtime })] }),
													entry.enabled ? (0, react_jsx_runtime.jsxs)("div", { children: [(0, react_jsx_runtime.jsx)("dt", { children: t("cordis") }), (0, react_jsx_runtime.jsx)("dd", { children: status })] }) : null
												]
											})]
										}) : null]
									}, entry.entryId);
								})
							}) : null
						]
					}) : null
				]
			});
		}
		//#endregion
		//#region lib/types/client/locales.js
		/** Copy dictionaries for the YunXi capability inventory Settings tab. */
		/** Simplified Chinese dictionary and key source of truth. */
		const zh = {
			tab: "能力开关",
			loading: "正在读取能力…",
			error: "暂时无法读取能力。",
			retry: "重试",
			search: "搜索能力",
			catalog: "能力列表",
			empty: "暂无能力。",
			emptySearch: "没有匹配的能力。",
			enabledTag: "已启用",
			disabledTag: "已停用",
			restartPendingTag: "重启后生效",
			coreTag: "核心",
			configuration: "下次启动",
			runtime: "当前运行",
			cordis: "进程状态",
			enable: "启用",
			disable: "停用",
			unavailable: "设置不可用",
			unobserved: "未启动",
			pending: "等待依赖",
			loadingPhase: "加载中",
			active: "运行中",
			failed: "启动失败",
			unloading: "正在停止"
		};
		/** English dictionary checked against the Chinese key set. */
		const en = {
			tab: "Capabilities",
			loading: "Reading capabilities…",
			error: "Capabilities are temporarily unavailable.",
			retry: "Retry",
			search: "Search capabilities",
			catalog: "Capabilities",
			empty: "No capabilities are available.",
			emptySearch: "No matching capabilities.",
			enabledTag: "Enabled",
			disabledTag: "Disabled",
			restartPendingTag: "Applies after restart",
			coreTag: "Core",
			configuration: "Next start",
			runtime: "Current runtime",
			cordis: "Process status",
			enable: "Enable",
			disable: "Disable",
			unavailable: "Settings unavailable",
			unobserved: "Not started",
			pending: "Waiting for dependencies",
			loadingPhase: "Loading",
			active: "Running",
			failed: "Start failed",
			unloading: "Stopping"
		};
		//#endregion
		//#region lib/types/client/index.js
		/** YunXi capability switches contributed through the dsh plugin inventory tab. */
		/** Dictionary namespace owned by this plugin. */
		const NS = "settings.pluginInventory";
		/** Services required by the Settings registration and generated Remote face. */
		const inject = [
			"slots",
			"locale",
			"remote",
			"remote.pluginInventory",
			"settingsScope"
		];
		const CAPABILITY_FIELDS = [
			"context",
			"persona",
			"memory",
			"companion",
			"storage",
			"mailbox",
			"scheduler",
			"shell",
			"patch",
			"files",
			"mcp",
			"skills"
		];
		/** Accept only the complete boolean section supplied by the Rust settings owner. */
		function decodeCapabilities(section) {
			if (typeof section !== "object" || section === null || Array.isArray(section)) return void 0;
			const candidate = section;
			if (!CAPABILITY_FIELDS.every((field) => typeof candidate[field] === "boolean")) return void 0;
			return candidate;
		}
		/** Contribute the inventory tab and bind its one persistent capability scope. */
		function apply(ctx) {
			ctx.effect(() => ctx.locale.register(NS, {
				zh,
				en
			}), "ui-settings-plugin-inventory: dictionaries");
			const capabilityScope = ctx.settingsScope.bind({
				namespace: "yunxi-capabilities",
				decode: decodeCapabilities
			});
			const t = ctx.locale.bind(NS);
			const list = async () => {
				const result = await ctx.remote.pluginInventory.list();
				if (!result.ok) throw new Error(`pluginInventory.list failed: ${result.error.code}: ${result.error.message}`);
				return result.value;
			};
			const getCapabilities = () => capabilityScope.getSnapshot();
			const subscribeCapabilities = (listener) => capabilityScope.subscribe(listener);
			const setCapability = (field, enabled) => capabilityScope.set(field, enabled);
			const injected = () => ({
				list,
				getCapabilities,
				subscribeCapabilities,
				setCapability
			});
			ctx.slots.inject("settings.plugins.tab", () => ctx.slots.register({
				name: "settings.plugins.tab",
				id: "all",
				order: 10,
				label: () => t("tab"),
				locale: NS,
				inject: injected
			}, PluginInventorySettingsTab));
		}
		//#endregion
		exports.NS = NS;
		exports.apply = apply;
		exports.inject = inject;
		return module.exports;
	}
});

//# sourceMappingURL=client.js.map