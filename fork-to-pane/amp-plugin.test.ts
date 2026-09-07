import { afterEach, beforeEach, expect, mock, test } from 'bun:test'
import type { PluginAPI, PluginCommandContext } from '@ampcode/plugin'
import plugin from './amp-plugin'

const id = 'T-aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee'
const environment = { HERDR_ENV: '1', HERDR_PANE_ID: 'w1:p1', HERDR_BIN_PATH: '/path with spaces/herdr', HERDR_SOCKET_PATH: '/tmp/herdr.sock' }
const installed = { plugin_id: 'gjermundgaraba.herdr-fork-to-pane', enabled: true, plugin_root: '/plugins with spaces/fork-to-pane' }
let saved: Record<string, string | undefined>

beforeEach(() => {
	saved = Object.fromEntries(Object.keys(environment).map(key => [key, process.env[key]]))
	Object.assign(process.env, environment)
})

afterEach(() => {
	for (const [key, value] of Object.entries(saved)) {
		if (value === undefined) delete process.env[key]
		else process.env[key] = value
	}
})

function setup(remote = false) {
	let handler: (ctx: PluginCommandContext) => void | Promise<void>
	const registerCommand = mock((_id: string, _options: unknown, callback: typeof handler) => { handler = callback })
	const run = mock(async (_file: string, _args: string[], _options: unknown) => ({ stdout: JSON.stringify({ result: { plugins: [installed] } }) }))
	const notify = mock(async (_message: string) => {})
	// No thread subscriptions, lifecycle hooks, or prompt APIs are provided.
	plugin({ registerCommand, system: { executor: { kind: remote ? 'remote' : 'local' } } } as unknown as PluginAPI, run)
	return { run, notify, registerCommand, invoke(threadID: string | null = id) {
		return handler({ thread: threadID === null ? undefined : { id: threadID }, ui: { notify } } as PluginCommandContext)
	} }
}

test.each(Object.keys(environment))('no command outside Herdr: missing %s', key => {
	delete process.env[key]
	expect(setup().registerCommand).not.toHaveBeenCalled()
})

test('no command on remote executors', () => {
	expect(setup(true).registerCommand).not.toHaveBeenCalled()
})

test('one-shot command uses invocation thread and the installed executable', async () => {
	const h = setup()
	expect(h.run).not.toHaveBeenCalled()
	expect(h.registerCommand.mock.calls[0][0]).toBe('branch-to-pane')
	await h.invoke()
	expect(h.run).toHaveBeenNthCalledWith(1, environment.HERDR_BIN_PATH,
		['plugin', 'list', '--plugin', installed.plugin_id, '--json'], { encoding: 'utf8', timeout: 5000 })
	expect(h.run).toHaveBeenNthCalledWith(2, `${installed.plugin_root}/bin/herdr-fork-to-pane`,
		['--amp-thread', id], { encoding: 'utf8', timeout: 45000 })
	const other = 'T-11111111-2222-4333-8444-555555555555'
	await h.invoke(other)
	expect(h.run.mock.calls[3][1]).toEqual(['--amp-thread', other])
	expect(h.notify).not.toHaveBeenCalled()
})

test('requires an existing thread', async () => {
	const h = setup()
	await h.invoke(null)
	expect(h.run).not.toHaveBeenCalled()
	expect(h.notify.mock.calls[0][0]).toContain('Open an existing Amp thread')
})

test.each([{ plugins: [] }, { plugins: [{ ...installed, enabled: false }] }])('requires an enabled installation %j', async ({ plugins }) => {
	const h = setup()
	h.run.mockResolvedValueOnce({ stdout: JSON.stringify({ result: { plugins } }) })
	await h.invoke()
	expect(h.run).toHaveBeenCalledTimes(1)
	expect(h.notify.mock.calls[0][0]).toContain('Install and enable')
})

test('reports launch failure without retrying', async () => {
	const h = setup()
	h.run.mockResolvedValueOnce({ stdout: JSON.stringify({ result: { plugins: [installed] } }) })
	h.run.mockRejectedValueOnce(new Error('launch outcome unknown'))
	await h.invoke()
	expect(h.run).toHaveBeenCalledTimes(2)
	expect(h.notify.mock.calls[0][0]).toContain('launch outcome unknown')
})
