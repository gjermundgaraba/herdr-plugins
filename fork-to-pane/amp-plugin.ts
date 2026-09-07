import type { PluginAPI } from '@ampcode/plugin'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'

export const description = 'Reports the active Amp thread to Herdr for Fork to Pane. Does not change or submit prompts.'

const execute = promisify(execFile)
const source = 'plugin:fork-to-pane:amp'
const isThreadID = (value: unknown): value is `T-${string}` =>
	typeof value === 'string' && value.length === 38 && /^T-[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/.test(value)

// The optional executor lets tests exercise the CLI contract without a live pane.
export default function (amp: PluginAPI, run: (
	file: string,
	args: string[],
	options: { encoding: 'utf8'; timeout: number },
) => Promise<{ stdout: string }> = execute) {
	const { HERDR_ENV, HERDR_PANE_ID: paneID, HERDR_BIN_PATH: binary, HERDR_SOCKET_PATH } = process.env
	if (HERDR_ENV !== '1' || !paneID || !binary || !HERDR_SOCKET_PATH || amp.system.executor.kind === 'remote') return

	let revision = 0
	let reports = Promise.resolve()

	function report(thread: { id: string } | null) {
		const currentRevision = ++revision
		// Serialize writes and skip superseded queued reports, including during disposal.
		reports = reports.then(async () => {
			if (currentRevision !== revision) return
			const id = isThreadID(thread?.id) ? thread.id : null
			await run(binary, [
				'pane', 'report-metadata', paneID, '--source', source,
				...(id === null ? ['--clear-token', 'amp_thread_id'] : ['--token', `amp_thread_id=${id}`]),
				'--ttl-ms', '30000',
			], { encoding: 'utf8', timeout: 1_000 })
		}).catch(error => amp.logger.log('Fork to Pane: could not report the active thread', error))
		return reports
	}

	void report(amp.activeThread.current)
	const activeSubscription = amp.activeThread.subscribe(thread => {
		void report(thread)
	})
	const heartbeat = setInterval(() => {
		void report(amp.activeThread.current)
	}, 10_000)
	heartbeat.unref()

	amp.onDispose(async () => {
		activeSubscription.unsubscribe()
		clearInterval(heartbeat)
		await report(null)
	})
}
