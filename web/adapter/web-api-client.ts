/** Browser API carrier for YunXi: HTTP unary calls plus recoverable SSE polling. */

import type { ApiProxy, HostFrame, MuxFrame, RpcRequest, ServerRequest } from './api.ts'
import { AbstractApiClient } from './api.ts'
import { hostFrameSchema, muxFrameSchema } from '@deepseek-ai/dsh-host-apiproxy/api/events.schema'
import { serverRequestSchema } from '@deepseek-ai/dsh-host-apiproxy/api/rpc.schema'
import { HOST_EVENTS_PATH, MUX_EVENTS_PATH } from '../api-path.ts'

/** Small client-side yield after a bounded long-poll response. */
const POLL_DELAY_MS = 5
const RETRY_DELAY_MS = 100
const SERVER_LONG_POLL_MS = 40
export const MAX_SSE_RESPONSE_BYTES = 512 * 1024
const MAX_CURSOR = Number.MAX_SAFE_INTEGER
type EventMethod = 'events.mux' | 'events.host'

/**
 * YunXi's Rust gateway deliberately closes each bounded SSE response. Keep the
 * logical dsh stream open by polling that carrier until the connection
 * generation is aborted.
 */
export class WebApiClient extends AbstractApiClient {
  private readonly eventCursors = new Map<string, number>()

  protected doFetch(input: URL, init?: RequestInit): Promise<Response> {
    return globalThis.fetch(input, init)
  }

  protected override openMux(
    _payload: Parameters<ApiProxy['events']['mux']>[0]['payload'],
    signal: AbortSignal,
    onOpen?: () => void,
  ): AsyncIterable<RpcRequest<MuxFrame>> {
    return this.pollSse(
      signal,
      opened => this.readRecoverableSse(MUX_EVENTS_PATH, 'events.mux', signal, muxFrameSchema, opened),
      onOpen,
    )
  }

  protected override openHost(
    _payload: Parameters<ApiProxy['events']['host']>[0]['payload'],
    signal: AbortSignal,
    onOpen?: () => void,
  ): AsyncIterable<RpcRequest<HostFrame>> {
    return this.pollSse(
      signal,
      opened => this.readRecoverableSse(HOST_EVENTS_PATH, 'events.host', signal, hostFrameSchema, opened),
      onOpen,
    )
  }

  private async *pollSse<F extends MuxFrame | HostFrame>(
    signal: AbortSignal,
    openStream: (onOpen: () => void) => AsyncIterable<RpcRequest<F>>,
    onOpen?: () => void,
  ): AsyncGenerator<RpcRequest<F>> {
    let opened = false
    while (!signal.aborted) {
      try {
        for await (const envelope of openStream(() => {
          if (opened) return
          opened = true
          onOpen?.()
        })) {
          yield envelope
        }
      } catch (error) {
        if (signal.aborted) return
        console.error('[client-connection] SSE poll failed; retrying:', error)
        await abortableDelay(RETRY_DELAY_MS, signal)
        continue
      }
      if (!signal.aborted) await abortableDelay(POLL_DELAY_MS, signal)
    }
  }

  /** Read one finite response and retain its transport cursor for the next poll. */
  private async *readRecoverableSse<F extends MuxFrame | HostFrame>(
    path: string,
    expectedMethod: EventMethod,
    signal: AbortSignal,
    frameSchema: { parse(value: unknown): F },
    onOpen?: () => void,
  ): AsyncGenerator<RpcRequest<F>> {
    const streamKey = path
    const cursor = this.eventCursors.get(streamKey) ?? 0
    const url = new URL(path, this.resolveBase())
    url.searchParams.set('afterSeq', String(cursor))
    url.searchParams.set('waitMs', String(SERVER_LONG_POLL_MS))
    const response = await this.doFetch(url, {
      signal,
      headers: {
        Accept: 'text/event-stream',
        'Last-Event-ID': String(cursor),
      },
    })
    if (!response.ok || response.body === null) {
      // A running browser can outlive a restarted local host. Its old cursor is
      // then ahead of the new journal; restart from the journal origin rather
      // than retrying a request the carrier will reject forever.
      if (response.status === 400) this.eventCursors.delete(streamKey)
      throw new Error(`transport failure for ${path}: HTTP ${response.status}`)
    }
    const declaredLength = response.headers.get('content-length')
    if (declaredLength !== null && Number(declaredLength) > MAX_SSE_RESPONSE_BYTES) {
      throw new Error(`SSE response for ${path} exceeds ${MAX_SSE_RESPONSE_BYTES} bytes`)
    }
    const replayGap = response.headers.get('x-yunxi-replay-gap') === 'true'
    const replayGapCursor = parseCursor(response.headers.get('x-yunxi-event-oldest'))
    onOpen?.()

    const reader = response.body.getReader()
    const decoder = new TextDecoder()
    let buffer = ''
    let receivedBytes = 0
    try {
      while (true) {
        const { done, value } = await reader.read()
        if (done) return
        receivedBytes += value.byteLength
        if (receivedBytes > MAX_SSE_RESPONSE_BYTES) {
          throw new Error(`SSE response for ${path} exceeds ${MAX_SSE_RESPONSE_BYTES} bytes`)
        }
        buffer += decoder.decode(value, { stream: true })
        if (buffer.length > MAX_SSE_RESPONSE_BYTES * 2) {
          throw new Error(`SSE response for ${path} has an unbounded frame`)
        }
        let boundary: number | undefined
        while ((boundary = sseBoundary(buffer)) !== undefined) {
          const chunk = buffer.slice(0, boundary)
          buffer = buffer.slice(boundary + sseBoundaryLength(buffer, boundary))
          const id = sseId(chunk)
          const currentCursor = this.eventCursors.get(streamKey) ?? 0
          if (id !== undefined && id <= currentCursor) continue
          const data = sseData(chunk)
          if (data === '') continue

          let full: ServerRequest
          let frame: F
          try {
            full = serverRequestSchema.parse(JSON.parse(data))
            frame = frameSchema.parse(full.payload)
            if (full.method !== expectedMethod) {
              throw new Error(`event method mismatch for ${path}: expected ${expectedMethod}, got ${full.method}`)
            }
          } catch (error) {
            console.error(`[client-connection] dropping malformed SSE frame on ${path}:`, error)
            advanceCursor(this.eventCursors, streamKey, id)
            continue
          }
          advanceCursor(this.eventCursors, streamKey, id)
          if (replayGap && replayGapCursor !== undefined && frame.type === 'stream/error') {
            advanceCursor(this.eventCursors, streamKey, replayGapCursor - 1)
          }
          this.onEnvelope(full)
          yield { rpcId: full.rpcId, payload: frame }
        }
      }
    } finally {
      await reader.cancel().catch(() => undefined)
    }
  }
}

export function sseId(chunk: string): number | undefined {
  const line = sseLines(chunk).find(value => value.startsWith('id:'))
  if (line === undefined) return undefined
  return parseCursor(line.slice(3).trim())
}

function sseData(chunk: string): string {
  return sseLines(chunk)
    .filter(line => line.startsWith('data:'))
    .map(line => {
      const value = line.slice(5)
      return value.startsWith(' ') ? value.slice(1) : value
    })
    .join('\n')
}

function sseLines(chunk: string): string[] {
  return chunk.split(/\r\n|\n|\r/)
}

function sseBoundary(buffer: string): number | undefined {
  const match = /\r\n\r\n|\n\n|\r\r/.exec(buffer)
  return match?.index
}

function sseBoundaryLength(buffer: string, boundary: number): number {
  const separator = buffer.slice(boundary).match(/^\r\n\r\n|^\n\n|^\r\r/)
  return separator?.[0].length ?? 0
}

export function advanceCursor(cursors: Map<string, number>, streamKey: string, id: number | undefined): void {
  if (id === undefined) return
  const current = cursors.get(streamKey) ?? 0
  if (id > current) cursors.set(streamKey, id)
}

function parseCursor(value: string | null): number | undefined {
  if (value === null || !/^\d+$/.test(value.trim())) return undefined
  const cursor = Number(value)
  return Number.isSafeInteger(cursor) && cursor > 0 && cursor <= MAX_CURSOR ? cursor : undefined
}

/** Resolve after one poll interval, or immediately when the stream is aborted. */
function abortableDelay(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const done = (): void => {
      clearTimeout(timer)
      signal.removeEventListener('abort', done)
      resolve()
    }
    const timer = setTimeout(done, ms)
    signal.addEventListener('abort', done, { once: true })
    if (signal.aborted) done()
  })
}
