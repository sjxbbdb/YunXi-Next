/** Copy dictionaries for the YunXi capability inventory Settings tab. */

/** Simplified Chinese dictionary and key source of truth. */
export const zh = {
  tab: '能力开关',
  loading: '正在读取能力…',
  error: '暂时无法读取能力。',
  writeError: '设置未保存，请稍后重试。',
  retry: '重试',
  search: '搜索能力',
  catalog: '能力列表',
  empty: '暂无能力。',
  emptySearch: '没有匹配的能力。',
  enabledTag: '已启用',
  disabledTag: '已停用',
  restartPendingTag: '重启后生效',
  coreTag: '核心',
  configuration: '下次启动',
  runtime: '当前运行',
  cordis: '进程状态',
  enable: '启用',
  disable: '停用',
  unavailable: '设置不可用',
  unobserved: '未启动',
  pending: '等待依赖',
  loadingPhase: '加载中',
  active: '运行中',
  failed: '启动失败',
  unloading: '正在停止',
} satisfies Record<string, string>

/** Locale key union shared with the English dictionary. */
export type PluginInventoryLocaleKey = keyof typeof zh

/** English dictionary checked against the Chinese key set. */
export const en = {
  tab: 'Capabilities',
  loading: 'Reading capabilities…',
  error: 'Capabilities are temporarily unavailable.',
  writeError: 'The setting was not saved. Try again later.',
  retry: 'Retry',
  search: 'Search capabilities',
  catalog: 'Capabilities',
  empty: 'No capabilities are available.',
  emptySearch: 'No matching capabilities.',
  enabledTag: 'Enabled',
  disabledTag: 'Disabled',
  restartPendingTag: 'Applies after restart',
  coreTag: 'Core',
  configuration: 'Next start',
  runtime: 'Current runtime',
  cordis: 'Process status',
  enable: 'Enable',
  disable: 'Disable',
  unavailable: 'Settings unavailable',
  unobserved: 'Not started',
  pending: 'Waiting for dependencies',
  loadingPhase: 'Loading',
  active: 'Running',
  failed: 'Start failed',
  unloading: 'Stopping',
} satisfies Record<PluginInventoryLocaleKey, string>
