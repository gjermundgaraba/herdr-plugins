import { afterEach, beforeEach, expect, mock, spyOn, test } from 'bun:test'
import type { PluginAPI } from '@ampcode/plugin'
import plugin from './amp-plugin'

const id = 'T-aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee'
const other = 'T-11111111-2222-4333-8444-555555555555'
const environment = { HERDR_ENV: '1', HERDR_PANE_ID: 'w1:p1', HERDR_BIN_PATH: '/path with spaces/herdr', HERDR_SOCKET_PATH: '/tmp/herdr.sock' }
let saved: Record<string, string | undefined>
let dispose: (() => Promise<void>) | undefined
let heartbeat: () => void
const interval = { unref() {} }
const flush = () => new Promise<void>(resolve => setImmediate(resolve))

beforeEach(() => {
	saved = Object.fromEntries(Object.keys(environment).map(key => [key, process.env[key]]))
	Object.assign(process.env, environment)
	dispose = undefined
	spyOn(globalThis, 'setInterval').mockImplementation(((callback: () => void, delay: number) => {
		expect(delay).toBe(10000)
		heartbeat = callback
		return interval
	}) as typeof setInterval)
	spyOn(globalThis, 'clearInterval').mockImplementation(() => {})
})

afterEach(async () => {
	await dispose?.()
	mock.restore()
	for (const [key, value] of Object.entries(saved)) {
		if (value === undefined) delete process.env[key]
		else process.env[key] = value
	}
})

function setup(initial: string | null = id, remote = false) {
	let changed: (thread: { id: string } | null) => void
	const unsubscribe = mock(() => {})
	const activeThread = {
		current: initial === null ? null : { id: initial },
		subscribe(callback: typeof changed) { changed = callback; return { unsubscribe } },
	}
	const on = mock(() => { throw new Error('Must not register prompt hooks') })
	const log = mock(() => {})
	const run = mock(async (_file: string, _args: string[], _options: unknown) => ({ stdout: '{}' }))
	plugin({ activeThread, on, logger: { log }, system: { executor: { kind: remote ? 'remote' : 'local' } }, onDispose(callback: typeof dispose) { dispose = callback } } as unknown as PluginAPI, run)
	return { run, log, on, unsubscribe, switch(value: string | null) {
		activeThread.current = value === null ? null : { id: value }
		changed(activeThread.current)
	} }
}

test.each(Object.keys(environment))('no-op without %s', async key => {
	delete process.env[key]
	const h = setup()
	await flush()
	expect(h.run).not.toHaveBeenCalled()
})

test('no-op on remote executors', async () => {
	const h = setup(id, true)
	await flush()
	expect(h.run).not.toHaveBeenCalled()
})

test('reports active identity, changes and null without any prompt hook', async () => {
	const h = setup()
	await flush()
	expect(h.run).toHaveBeenLastCalledWith(environment.HERDR_BIN_PATH, [
		'pane', 'report-metadata', 'w1:p1', '--source', 'plugin:fork-to-pane:amp',
		'--token', `amp_thread_id=${id}`, '--ttl-ms', '30000',
	], { encoding: 'utf8', timeout: 1000 })
	h.switch(other)
	await flush()
	expect(h.run.mock.calls.at(-1)![1]).toContain(`amp_thread_id=${other}`)
	h.switch(null)
	await flush()
	expect(h.run.mock.calls.at(-1)![1]).toContain('--clear-token')
	expect(h.on).not.toHaveBeenCalled()
})

test.each([null, 'invalid', `${id}\n`])('clears invalid or missing identity %j', async value => {
	const h = setup(value)
	await flush()
	expect(h.run.mock.calls.at(-1)![1]).toContain('--clear-token')
})

test('refreshes identity and stops reporting on disposal', async () => {
	const h = setup()
	await flush()
	heartbeat()
	await flush()
	expect(h.run).toHaveBeenCalledTimes(2)
	await dispose!()
	dispose = undefined
	expect(h.unsubscribe).toHaveBeenCalled()
	expect(clearInterval).toHaveBeenCalledWith(interval)
	expect(h.run.mock.calls.at(-1)![1]).toContain('--clear-token')
	expect(h.run).toHaveBeenCalledTimes(3)
})

test('skips superseded queued reports and recovers from CLI errors', async () => {
	const h = setup()
	h.switch(other)
	await flush()
	expect(h.run).toHaveBeenCalledTimes(1)
	expect(h.run.mock.calls[0][1]).toContain(`amp_thread_id=${other}`)
	h.run.mockRejectedValueOnce(new Error('offline'))
	heartbeat()
	await flush()
	expect(h.log).toHaveBeenCalled()
	heartbeat()
	await flush()
	expect(h.run).toHaveBeenCalledTimes(3)
})
