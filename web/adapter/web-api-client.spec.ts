import { describe, expect, it, vi } from 'vitest'
import { advanceCursor, MAX_SSE_RESPONSE_BYTES, sseId, WebApiClient } from './web-api-client.ts'

class TestWebApiClient extends WebApiClient {
  readonly requests: Array<{ url: URL; init?: RequestInit }> = []
  private readonly responses: Array<Response | ((request: { url: URL; init?: RequestInit }) => Response)>

  constructor(responses: Array<Response | ((request: { url: URL; init?: RequestInit }) => Response)>) {
    super()
    this.responses = responses
  }

  protected override doFetch(input: URL, init?: RequestInit): Promise<Response> {
    this.requests.push({ url: input, init })
    const response = this.responses.shift() ?? sseResponse('')
    return Promise.resolve(typeof response === 'function' ? response({ url: input, init }) : response)
  }
}

function sseResponse(data: string): Response {
  return new Response(data, { headers: { 'content-type': 'text/event-stream' } })
}

function jsonResponse(value: unknown): Response {
  return new Response(JSON.stringify(value), { headers: { 'content-type': 'application/json' } })
}

function eventEnvelope(method: 'events.mux' | 'events.host', rpcId: string, payload: unknown): string {
  return JSON.stringify({ type: 'server-request', rpcId, method, payload })
}

describe('YunXi SSE cursor adapter', () => {
  it('accepts numeric SSE ids and rejects unsafe or malformed ids', () => {
    expect(sseId('id: 42\ndata: {}')).toBe(42)
    expect(sseId('id:\ndata: {}')).toBeUndefined()
    expect(sseId('id: -1\ndata: {}')).toBeUndefined()
    expect(sseId(`id: ${Number.MAX_SAFE_INTEGER + 1}\ndata: {}`)).toBeUndefined()
    expect(sseId('id: 43\rdata: {}')).toBe(43)
  })

  it('keeps cursors monotone and isolated by stream path', () => {
    const cursors = new Map<string, number>()
    advanceCursor(cursors, '/api/events.mux', 8)
    advanceCursor(cursors, '/api/events.mux', 3)
    advanceCursor(cursors, '/api/events.host', 2)
    expect(cursors).toEqual(new Map([
      ['/api/events.mux', 8],
      ['/api/events.host', 2],
    ]))
  })

  it('resumes finite responses with both cursor forms and bounded long-polling', async () => {
    const frame = JSON.stringify({
      type: 'server-request',
      rpcId: 'event-1',
      method: 'events.mux',
      payload: { type: 'session/subscribed', sessionId: 'session-1', lastSeq: 0 },
    })
    const client = new TestWebApiClient([
      sseResponse(`id: 7\ndata: ${frame}\n\n`),
      sseResponse(''),
    ])
    const abort = new AbortController()
    const iterator = client.events.mux({}, abort.signal)[Symbol.asyncIterator]()

    const first = await iterator.next()
    expect(first.done).toBe(false)
    expect(first.value?.payload).toMatchObject({ type: 'session/subscribed' })
    expect(client.requests[0]?.url.searchParams.get('afterSeq')).toBe('0')
    expect(client.requests[0]?.url.searchParams.get('waitMs')).toBe('40')
    expect(client.requests[0]?.init?.headers).toMatchObject({ 'Last-Event-ID': '0' })

    const next = iterator.next()
    await vi.waitFor(() => expect(client.requests.length).toBeGreaterThanOrEqual(2))
    expect(client.requests[1]?.url.searchParams.get('afterSeq')).toBe('7')
    expect(client.requests[1]?.init?.headers).toMatchObject({ 'Last-Event-ID': '7' })
    abort.abort()
    await next
  })

  it('maps unary RPC and response routes through the inherited dsh carrier', async () => {
    const client = new TestWebApiClient([
      request => {
        const body = JSON.parse(String(request.init?.body)) as { rpcId: string }
        return jsonResponse({
          type: 'server-response',
          rpcId: body.rpcId,
          result: { ok: true, value: {
            version: '0.1.0', cwd: 'D:/work', attachedSessions: 0,
            home: 'D:/home', canOpenPath: false,
          } },
        })
      },
      jsonResponse({ accepted: true }),
    ])

    const described = await client.host.describe({})
    expect(described.result).toMatchObject({ ok: true, value: { version: '0.1.0' } })
    const response = { type: 'client-response', rpcId: 'approval-1' as never, result: { ok: true, value: {} } } as never
    await expect(client.respond(response)).resolves.toEqual({ accepted: true })
    expect(client.requests[0]?.url.pathname).toBe('/api/host.describe')
    expect(client.requests[0]?.init?.method).toBe('POST')
    expect(client.requests[1]?.url.pathname).toBe('/api/respond')
    expect(JSON.parse(String(client.requests[1]?.init?.body))).toMatchObject({
      type: 'client-response', rpcId: 'approval-1',
    })
  })

  it('reads CRLF and split data lines from the host event channel', async () => {
    const frame = eventEnvelope('events.host', 'event-host-1', {
      type: 'host/session-status', sessionId: 'session-host', running: true,
    })
    const split = frame.indexOf(',"payload"')
    const response = sseResponse(`: connected\r\n\r\nid: 11\r\ndata: ${frame.slice(0, split)}\r\ndata: ${frame.slice(split)}\r\n\r\n: end\r\n\r\n`)
    const client = new TestWebApiClient([response])
    const abort = new AbortController()
    const opened = vi.fn()
    const result = await client.events.host({}, abort.signal, opened)[Symbol.asyncIterator]().next()

    expect(result.value?.payload).toEqual({ type: 'host/session-status', sessionId: 'session-host', running: true })
    expect(client.requests[0]?.url.pathname).toBe('/api/events.host')
    expect(opened).toHaveBeenCalledOnce()
    abort.abort()
  })

  it('drops frames from the wrong event channel and advances past malformed frames', async () => {
    const wrongChannel = eventEnvelope('events.host', 'event-wrong', {
      type: 'session/subscribed', sessionId: 'session-wrong', lastSeq: 1,
    })
    const malformed = eventEnvelope('events.mux', 'event-bad', { type: 'unknown' })
    const valid = eventEnvelope('events.mux', 'event-good', {
      type: 'session/subscribed', sessionId: 'session-mux', lastSeq: 3,
    })
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => undefined)
    const client = new TestWebApiClient([sseResponse(
      `id: 1\ndata: ${wrongChannel}\n\nid: 2\ndata: ${malformed}\n\nid: 3\ndata: ${valid}\n\n`,
    )])
    const abort = new AbortController()
    const result = await client.events.mux({}, abort.signal)[Symbol.asyncIterator]().next()

    expect(result.value?.payload).toEqual({ type: 'session/subscribed', sessionId: 'session-mux', lastSeq: 3 })
    expect(errorSpy).toHaveBeenCalledTimes(2)
    abort.abort()
    errorSpy.mockRestore()
  })

  it('delivers replay-gap controls without skipping the retained frame', async () => {
    const gap = eventEnvelope('events.mux', 'event-replay-gap', {
      type: 'stream/error', error: { code: 'internal', message: 'replay gap', details: {} },
    })
    const retained = eventEnvelope('events.mux', 'event-retained', {
      type: 'session/subscribed', sessionId: 'session-replay', lastSeq: 4,
    })
    const client = new TestWebApiClient([new Response(
      `id: 4\ndata: ${gap}\n\nid: 5\ndata: ${retained}\n\n`,
      { headers: { 'content-type': 'text/event-stream', 'x-yunxi-replay-gap': 'true', 'x-yunxi-event-oldest': '5' } },
    )])
    const abort = new AbortController()
    const iterator = client.events.mux({}, abort.signal)[Symbol.asyncIterator]()

    expect((await iterator.next()).value?.payload).toMatchObject({ type: 'stream/error' })
    expect((await iterator.next()).value?.payload).toMatchObject({ type: 'session/subscribed', lastSeq: 4 })
    abort.abort()
  })

  it('suppresses duplicate ids on a resumed poll', async () => {
    const first = eventEnvelope('events.mux', 'event-first', {
      type: 'session/subscribed', sessionId: 'session-reconnect', lastSeq: 1,
    })
    const duplicate = eventEnvelope('events.mux', 'event-duplicate', {
      type: 'session/subscribed', sessionId: 'session-reconnect', lastSeq: 1,
    })
    const next = eventEnvelope('events.mux', 'event-next', {
      type: 'session/subscribed', sessionId: 'session-reconnect', lastSeq: 2,
    })
    const client = new TestWebApiClient([
      sseResponse(`id: 7\ndata: ${first}\n\n`),
      sseResponse(`id: 7\ndata: ${duplicate}\n\nid: 8\ndata: ${next}\n\n`),
    ])
    const abort = new AbortController()
    const iterator = client.events.mux({}, abort.signal)[Symbol.asyncIterator]()
    expect((await iterator.next()).value?.payload).toMatchObject({ lastSeq: 1 })
    expect((await iterator.next()).value?.payload).toMatchObject({ lastSeq: 2 })
    abort.abort()
  })

  it('restarts from zero after the server rejects a stale cursor', async () => {
    const beforeRestart = eventEnvelope('events.mux', 'event-before-restart', {
      type: 'session/subscribed', sessionId: 'session-restart', lastSeq: 7,
    })
    const afterRestart = eventEnvelope('events.mux', 'event-after-restart', {
      type: 'session/subscribed', sessionId: 'session-restart', lastSeq: 0,
    })
    const client = new TestWebApiClient([
      sseResponse(`id: 7\ndata: ${beforeRestart}\n\n`),
      new Response(null, { status: 400 }),
      sseResponse(`id: 1\ndata: ${afterRestart}\n\n`),
    ])
    const abort = new AbortController()
    const iterator = client.events.mux({}, abort.signal)[Symbol.asyncIterator]()
    expect((await iterator.next()).value?.payload).toMatchObject({ lastSeq: 7 })
    const pending = iterator.next()

    await vi.waitFor(() => expect(client.requests).toHaveLength(3), { timeout: 1000 })
    expect(client.requests[1]?.url.searchParams.get('afterSeq')).toBe('7')
    expect(client.requests[2]?.url.searchParams.get('afterSeq')).toBe('0')
    expect((await pending).value?.payload).toMatchObject({ lastSeq: 0 })
    abort.abort()
  })

  it('retries an oversized finite response instead of accepting an unbounded body', async () => {
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => undefined)
    const client = new TestWebApiClient([
      sseResponse('x'.repeat(MAX_SSE_RESPONSE_BYTES + 1)),
      sseResponse(''),
    ])
    const abort = new AbortController()
    const pending = client.events.mux({}, abort.signal)[Symbol.asyncIterator]().next()
    await vi.waitFor(() => expect(client.requests).toHaveLength(2), { timeout: 1000 })
    expect(errorSpy).toHaveBeenCalled()
    abort.abort()
    await pending
    errorSpy.mockRestore()
  })

  it('retries a dropped poll with the last confirmed cursor', async () => {
    const frame = JSON.stringify({
      type: 'server-request',
      rpcId: 'event-2',
      method: 'events.mux',
      payload: { type: 'session/subscribed', sessionId: 'session-2', lastSeq: 1 },
    })
    const abort = new AbortController()
    const client = new TestWebApiClient([
      new Response(null, { status: 503 }),
      sseResponse(`id: 9\ndata: ${frame}\n\n`),
    ])
    const iterator = client.events.mux({}, abort.signal)[Symbol.asyncIterator]()

    const pending = iterator.next()
    await vi.waitFor(() => expect(client.requests).toHaveLength(2), { timeout: 1000 })
    expect(client.requests[1]?.url.searchParams.get('afterSeq')).toBe('0')
    expect(client.requests[1]?.init?.headers).toMatchObject({ 'Last-Event-ID': '0' })
    const result = await pending
    expect(result.value?.payload).toMatchObject({ sessionId: 'session-2' })
    abort.abort()
  })
})
