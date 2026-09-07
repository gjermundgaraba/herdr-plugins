import type { PluginAPI } from '@ampcode/plugin'
import { execFile } from 'node:child_process'
import { join } from 'node:path'
import { promisify } from 'node:util'

export const description = 'Branch the current Amp thread into a right-hand Herdr pane with an editable thread reference. Never submits a prompt.'

const execute = promisify(execFile)
const pluginID = 'gjermundgaraba.herdr-fork-to-pane'

// The optional executor lets tests exercise the CLI contract without a live pane.
export default function (amp: PluginAPI, run: (
	file: string,
	args: string[],
	options: { encoding: 'utf8'; timeout: number },
) => Promise<{ stdout: string }> = execute) {
	const { HERDR_ENV, HERDR_PANE_ID, HERDR_BIN_PATH: binary, HERDR_SOCKET_PATH } = process.env
	if (HERDR_ENV !== '1' || !HERDR_PANE_ID || !binary || !HERDR_SOCKET_PATH || amp.system.executor.kind === 'remote') return

	amp.registerCommand('branch-to-pane', {
		title: 'Branch into right pane',
		category: 'Herdr',
		description: 'Open Amp in a new pane with an editable reference to this thread, without submitting it.',
	}, async ctx => {
		if (!ctx.thread) {
			await ctx.ui.notify('Open an existing Amp thread before branching into a pane.')
			return
		}
		try {
			const result = await run(binary, ['plugin', 'list', '--plugin', pluginID, '--json'], { encoding: 'utf8', timeout: 5_000 })
			const plugin = JSON.parse(result.stdout).result.plugins.find((entry: { plugin_id: string }) => entry.plugin_id === pluginID)
			if (!plugin?.enabled || !plugin.plugin_root) {
				throw new Error('Install and enable the Herdr Fork to Pane plugin first.')
			}
			await run(join(plugin.plugin_root, 'bin/herdr-fork-to-pane'), ['--amp-thread', ctx.thread.id], { encoding: 'utf8', timeout: 45_000 })
		} catch (error) {
			await ctx.ui.notify(`Could not branch into a pane: ${error instanceof Error ? error.message : String(error)}`)
		}
	})
}
