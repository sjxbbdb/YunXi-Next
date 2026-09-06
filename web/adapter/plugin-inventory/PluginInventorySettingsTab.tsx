import {
  useEffect, useId, useMemo, useState, useSyncExternalStore, type ReactNode,
} from 'react'
import type { PluginInventorySnapshot } from '@deepseek-ai/dsh-api-remotes/client'
import {
  IconChevronDownOutline14,
  IconSearchOutline16,
} from '@deepseek-ai/dsh-client-ui-primitives'
import type { InjectFace, PropsLocale, PropsRuntime } from '@deepseek-ai/dsh-client-ui-slots'
import type { PluginInventoryLocaleKey } from './locales.ts'
import css from './PluginInventorySettingsTab.module.css'

export type CapabilityField =
  | 'context'
  | 'persona'
  | 'memory'
  | 'companion'
  | 'storage'
  | 'mailbox'
  | 'scheduler'
  | 'shell'
  | 'patch'
  | 'files'
  | 'mcp'
  | 'skills'
  | 'multi_agent'
  | 'voice'
  | 'weixin'

export type CapabilitySettings = Record<CapabilityField, boolean>

export interface CapabilitySettingsSnapshot {
  status: 'loading' | 'ready' | 'unavailable'
  value: CapabilitySettings | undefined
  writable: boolean
}

/** Registration-side Remote and settings faces used by the section. */
export interface PluginInventorySettingsTabInjected {
  /** Read a current Host inventory snapshot. */
  list: () => Promise<PluginInventorySnapshot>
  /** Read the stable snapshot from the shared dsh settings mirror. */
  getCapabilities: () => CapabilitySettingsSnapshot
  /** Subscribe to persistent capability-view replacements. */
  subscribeCapabilities: (listener: () => void) => () => void
  /** Persist one composition-scoped capability choice. */
  setCapability: (field: CapabilityField, enabled: boolean) => Promise<void>
  /** Persist an arbitrary user-installed optional plugin choice. */
  setPlugin: (pluginId: string, enabled: boolean) => Promise<void>
}

export type PluginInventorySettingsTabProps =
  PropsRuntime<'settings.plugins.tab'>
  & PropsLocale<'settings.pluginInventory'>
  & InjectFace<PluginInventorySettingsTabInjected>

type PluginInventoryEntry = PluginInventorySnapshot['entries'][number]
type PluginFiberPhase = PluginInventoryEntry['fiberPhase']

type ViewState =
  | { readonly status: 'loading' }
  | { readonly status: 'error' }
  | { readonly status: 'ready'; readonly snapshot: PluginInventorySnapshot }

const PHASE_KEYS = {
  pending: 'pending',
  loading: 'loadingPhase',
  active: 'active',
  failed: 'failed',
  unloading: 'unloading',
} satisfies Record<Exclude<PluginFiberPhase, null>, PluginInventoryLocaleKey>

const CAPABILITY_BY_ENTRY: Readonly<Record<string, CapabilityField>> = {
  'yunxi.context': 'context',
  'yunxi.persona': 'persona',
  'yunxi.memory': 'memory',
  'yunxi.companion': 'companion',
  'yunxi.storage': 'storage',
  'yunxi.companion-mailbox': 'mailbox',
  'yunxi.scheduler': 'scheduler',
  'yunxi.tool.shell': 'shell',
  'yunxi.tool.patch': 'patch',
  'yunxi.tool.files': 'files',
  'yunxi.tool.mcp': 'mcp',
  'yunxi.tool.skills': 'skills',
  'yunxi.multi-agent': 'multi_agent',
  'yunxi.voice.fixture': 'voice',
  'yunxi.channel.weixin': 'weixin',
}

function phaseLabel(
  phase: PluginFiberPhase,
  t: PluginInventorySettingsTabProps['t'],
): string {
  return phase === null ? t('unobserved') : t(PHASE_KEYS[phase])
}

function moduleShortName(moduleName: string): string {
  const unscoped = moduleName.startsWith('@') ? moduleName.slice(moduleName.indexOf('/') + 1) : moduleName
  return unscoped
    .replace(/^cordis:/, '')
    .replace(/^cordis-plugin-/, '')
    .replace(/^dsh-(?:host-|client-)?/, '')
    .replace(/^yunxi\.plugin\.yunxi\./, '')
}

function matches(entry: PluginInventoryEntry, normalizedQuery: string): boolean {
  if (normalizedQuery.length === 0) return true
  return [entry.moduleName, entry.entryId]
    .some(value => value.toLocaleLowerCase().includes(normalizedQuery))
}

/** Render the current inventory with composition-scoped YunXi capability switches. */
export function PluginInventorySettingsTab({
  list,
  getCapabilities,
  subscribeCapabilities,
  setCapability,
  setPlugin,
  t,
}: PluginInventorySettingsTabProps): ReactNode {
  const catalogId = useId()
  const [request, setRequest] = useState(0)
  const [query, setQuery] = useState('')
  const [expanded, setExpanded] = useState<PluginInventoryEntry['entryId'] | null>(null)
  const [state, setState] = useState<ViewState>({ status: 'loading' })
  const [savingFields, setSavingFields] = useState<ReadonlySet<string>>(() => new Set())
  const [writeError, setWriteError] = useState(false)
  const capabilities = useSyncExternalStore(
    subscribeCapabilities,
    getCapabilities,
    getCapabilities,
  )

  useEffect(() => {
    let current = true
    void Promise.resolve().then(() => list()).then(
      (snapshot) => { if (current) setState({ status: 'ready', snapshot }) },
      () => { if (current) setState({ status: 'error' }) },
    )
    return () => { current = false }
  }, [list, request])

  const normalizedQuery = query.trim().toLocaleLowerCase()
  const filteredEntries = useMemo(
    () => state.status === 'ready'
      ? state.snapshot.entries.filter(entry => matches(entry, normalizedQuery))
      : [],
    [normalizedQuery, state],
  )

  useEffect(() => {
    if (expanded !== null && !filteredEntries.some(entry => entry.entryId === expanded)) {
      setExpanded(null)
    }
  }, [expanded, filteredEntries])

  const retry = (): void => {
    setState({ status: 'loading' })
    setRequest(value => value + 1)
  }

  const chooseCapability = (field: CapabilityField, enabled: boolean): void => {
    setWriteError(false)
    setSavingFields(previous => new Set([...previous, field]))
    void setCapability(field, enabled)
      .catch(() => { setWriteError(true) })
      .finally(() => {
        setSavingFields((previous) => {
          const next = new Set(previous)
          next.delete(field)
          return next
        })
      })
  }

  const choosePlugin = (pluginId: string, enabled: boolean): void => {
    setWriteError(false)
    setSavingFields(previous => new Set([...previous, pluginId]))
    void setPlugin(pluginId, enabled)
      .then(() => { setRequest(value => value + 1) })
      .catch(() => { setWriteError(true) })
      .finally(() => {
        setSavingFields((previous) => {
          const next = new Set(previous)
          next.delete(pluginId)
          return next
        })
      })
  }

  return (
    <div className={css.section} aria-busy={state.status === 'loading'}>
      {state.status === 'loading' ? <p className={css.status}>{t('loading')}</p> : null}
      {state.status === 'error' ? (
        <div className={css.failure}>
          <p role="alert">{t('error')}</p>
          <button type="button" onClick={retry}>{t('retry')}</button>
        </div>
      ) : null}
      {state.status === 'ready' ? (
        <div className={css.catalog}>
          {writeError ? <p className={css.writeError} role="alert">{t('writeError')}</p> : null}
          <label className={css.search}>
            <IconSearchOutline16 aria-hidden="true" />
            <span className={css.visuallyHidden}>{t('search')}</span>
            <input
              type="search"
              value={query}
              placeholder={t('search')}
              aria-label={t('search')}
              onChange={(event) => { setQuery(event.currentTarget.value) }}
            />
          </label>
          <div className={css.catalogHeading}>
            <h3>{t('catalog')}</h3>
            <span data-plugin-count={filteredEntries.length}>{filteredEntries.length}</span>
          </div>
          {state.snapshot.entries.length === 0 ? <p className={css.status}>{t('empty')}</p> : null}
          {state.snapshot.entries.length > 0 && filteredEntries.length === 0
            ? <p className={css.status}>{t('emptySearch')}</p>
            : null}
          {filteredEntries.length > 0 ? (
            <ul className={css.cards}>
              {filteredEntries.map((entry) => {
                const status = phaseLabel(entry.fiberPhase, t)
                const title = moduleShortName(entry.moduleName)
                const field = CAPABILITY_BY_ENTRY[String(entry.entryId)]
                // The Rust Gateway removes local manifest metadata from the
                // strict dsh inventory response. The reserved module prefix
                // is the stable wire-level marker for user-installed entries.
                const dynamicToggleable = field === undefined
                  && entry.moduleName.startsWith('yunxi.dynamic.')
                const configured = field === undefined
                  ? entry.enabled
                  : capabilities.value?.[field] ?? entry.enabled
                const restartPending = (field !== undefined || dynamicToggleable) && configured !== entry.enabled
                const configuration = field === undefined && !dynamicToggleable
                  ? t('coreTag')
                  : restartPending
                    ? t('restartPendingTag')
                    : t(configured ? 'enabledTag' : 'disabledTag')
                const runtime = t(entry.enabled ? 'enabledTag' : 'disabledTag')
                const open = expanded === entry.entryId
                const detailId = `${catalogId}-details-${encodeURIComponent(entry.entryId)}`
                const saving = savingFields.has(field ?? String(entry.entryId))
                const canWrite = capabilities.status === 'ready'
                  && capabilities.writable
                  && (field !== undefined || dynamicToggleable)
                  && !saving
                const toggleLabel = field === undefined && !dynamicToggleable
                  ? t('unavailable')
                  : t(configured ? 'disable' : 'enable')
                return (
                  <li
                    className={css.card}
                    key={entry.entryId}
                    data-plugin-entry={entry.entryId}
                    data-open={open ? 'true' : undefined}
                    data-pending-restart={restartPending ? 'true' : undefined}
                  >
                    <div className={css.cardHeader}>
                      <button
                        className={css.cardContent}
                        type="button"
                        aria-expanded={open}
                        aria-controls={detailId}
                        aria-label={`${title}, ${status}, ${configuration}`}
                        onClick={() => {
                          setExpanded(current => current === entry.entryId ? null : entry.entryId)
                        }}
                      >
                        <strong className={css.cardTitle} title={entry.moduleName}>{title}</strong>
                        <span className={css.cardTrailing}>
                          {entry.enabled ? (
                            <span
                              className={css.statusDot}
                              data-phase={entry.fiberPhase ?? 'unobserved'}
                              role="img"
                              aria-label={status}
                              title={status}
                            />
                          ) : null}
                          <span
                            className={css.configTag}
                            data-enabled={configured ? 'true' : 'false'}
                            data-pending={restartPending ? 'true' : undefined}
                          >
                            {configuration}
                          </span>
                          <IconChevronDownOutline14 className={css.chevron} size={12} aria-hidden="true" />
                        </span>
                      </button>
                      {field !== undefined || dynamicToggleable ? (
                        <button
                          className={css.capabilitySwitch}
                          type="button"
                          role="switch"
                          aria-checked={configured}
                          aria-label={`${toggleLabel} ${title}`}
                          title={`${toggleLabel} ${title}`}
                          data-capability-field={field}
                          data-plugin-id={field === undefined ? entry.entryId : undefined}
                          data-checked={configured ? 'true' : 'false'}
                          disabled={!canWrite}
                          onClick={() => {
                            if (field !== undefined) chooseCapability(field, !configured)
                            else choosePlugin(String(entry.entryId), !configured)
                          }}
                        >
                          <span aria-hidden="true" />
                        </button>
                      ) : null}
                    </div>
                    {open ? (
                      <div className={css.cardDetails} id={detailId}>
                        <code className={css.entryValue} data-loader-entry>{entry.entryId}</code>
                        <dl className={css.details}>
                          <div>
                            <dt>{t('configuration')}</dt>
                          <dd>{field === undefined && !dynamicToggleable ? t('coreTag') : t(configured ? 'enabledTag' : 'disabledTag')}</dd>
                          </div>
                          <div>
                            <dt>{t('runtime')}</dt>
                            <dd>{runtime}</dd>
                          </div>
                          {entry.enabled ? (
                            <div>
                              <dt>{t('cordis')}</dt>
                              <dd>{status}</dd>
                            </div>
                          ) : null}
                        </dl>
                      </div>
                    ) : null}
                  </li>
                )
              })}
            </ul>
          ) : null}
        </div>
      ) : null}
    </div>
  )
}
