/** YunXi capability switches contributed through the dsh plugin inventory tab. */

import type {} from '@deepseek-ai/dsh-client-locale/client'
import type { ClientContext } from '@deepseek-ai/dsh-client-runtime/client'
import type {} from '@deepseek-ai/dsh-client-ui-settings/client'
import {
  PluginInventorySettingsTab,
  type CapabilityField,
  type CapabilitySettings,
  type PluginInventorySettingsTabInjected,
} from './PluginInventorySettingsTab.tsx'
import { en, zh, type PluginInventoryLocaleKey } from './locales.ts'

export type {
  CapabilityField,
  CapabilitySettings,
  PluginInventorySettingsTabInjected,
  PluginInventorySettingsTabProps,
} from './PluginInventorySettingsTab.tsx'
export type { PluginInventoryLocaleKey } from './locales.ts'

declare module '@deepseek-ai/dsh-client-ui-slots' {
  interface LocaleNamespaceMap {
    /** Host plugin inventory and restart-scoped YunXi capability controls. */
    'settings.pluginInventory': PluginInventoryLocaleKey
  }
}

/** Dictionary namespace owned by this plugin. */
export const NS = 'settings.pluginInventory'

/** Services required by the Settings registration and generated Remote face. */
export const inject = ['slots', 'locale', 'remote', 'remote.pluginInventory', 'settingsScope']

const CAPABILITY_FIELDS: readonly CapabilityField[] = [
  'context', 'persona', 'memory', 'companion', 'storage', 'mailbox',
  'scheduler', 'shell', 'patch', 'files', 'mcp', 'skills',
  'multi_agent',
]

/** Accept only the complete boolean section supplied by the Rust settings owner. */
function decodeCapabilities(section: unknown): CapabilitySettings | undefined {
  if (typeof section !== 'object' || section === null || Array.isArray(section)) return undefined
  const candidate = section as Record<string, unknown>
  if (!CAPABILITY_FIELDS.every(field => typeof candidate[field] === 'boolean')) return undefined
  return candidate as unknown as CapabilitySettings
}

/** Contribute the inventory tab and bind its one persistent capability scope. */
export function apply(ctx: ClientContext): void {
  ctx.effect(() => ctx.locale.register(NS, { zh, en }), 'ui-settings-plugin-inventory: dictionaries')

  const capabilityScope = ctx.settingsScope.bind<CapabilitySettings>({
    namespace: 'yunxi-capabilities',
    decode: decodeCapabilities,
  })
  const t = ctx.locale.bind(NS)
  const list: PluginInventorySettingsTabInjected['list'] = async () => {
    const result = await ctx.remote.pluginInventory.list()
    if (!result.ok) {
      throw new Error(`pluginInventory.list failed: ${result.error.code}: ${result.error.message}`)
    }
    return result.value
  }
  const getCapabilities: PluginInventorySettingsTabInjected['getCapabilities'] = () => capabilityScope.getSnapshot()
  const subscribeCapabilities: PluginInventorySettingsTabInjected['subscribeCapabilities'] = listener => capabilityScope.subscribe(listener)
  const setCapability: PluginInventorySettingsTabInjected['setCapability'] = (field, enabled) => capabilityScope.set(field, enabled)
  const injected = (): PluginInventorySettingsTabInjected => ({
    list,
    getCapabilities,
    subscribeCapabilities,
    setCapability,
  })

  ctx.slots.inject('settings.plugins.tab', () => ctx.slots.register({
    name: 'settings.plugins.tab',
    id: 'all',
    order: 10,
    label: () => t('tab'),
    locale: NS,
    inject: injected,
  }, PluginInventorySettingsTab))
}
