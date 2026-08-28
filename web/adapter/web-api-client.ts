/** Browser API carrier for YunXi: HTTP unary calls plus bounded SSE polling. */

import type { ApiProxy, HostFrame, MuxFrame, RpcRequest } from './api.ts'
import { AbstractApiClient } from './api.ts'
import { hostFrameSchema, muxFrameSchema } from '@deepseek-ai/dsh-host-apiproxy/api/events.schema'
import { HOST_EVENTS_PATH, MUX_EVENTS_PATH } from '../api-path.ts'

/** Delay between bounded event responses so an idle browser does not busy-loop. */
const POLL_DELAY_MS = 250

/**
 * YunXi's Rust gateway deliberately closes each bounded SSE response. Keep the
 * logical dsh stream open by polling that carrier until the connection
 * generation is aborted.
 */
export class WebApiClient extends AbstractApiClient {
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
      opened => this.readSse(MUX_EVENTS_PATH, signal, muxFrameSchema, opened),
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
      opened => this.readSse(HOST_EVENTS_PATH, signal, hostFrameSchema, opened),
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
      for await (const envelope of openStream(() => {
        if (opened) return
        opened = true
        onOpen?.()
      })) {
        yield envelope
      }
      if (!signal.aborted) await abortableDelay(POLL_DELAY_MS, signal)
    }
  }
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
