import { invoke } from '@tauri-apps/api/core';
import { useAccountStore } from '@/app/stores/accountStore';
import type { MoveTask } from '@/app/stores/moveStore';
import {
  buildSourceConfigFromAccounts,
  buildDestinationConfigFromAccounts,
} from '@/app/utils/moveConfig';

/** Install current credentials before making a persisted task runnable. */
export async function resumeSavedMove(task: MoveTask): Promise<void> {
  await useAccountStore.getState().loadAccounts();
  const accounts = useAccountStore.getState().accounts;
  const sourceConfig = buildSourceConfigFromAccounts(task, accounts);
  const destConfig = buildDestinationConfigFromAccounts(task, accounts);
  if (!sourceConfig || !destConfig)
    throw new Error('Source and destination credentials are required to resume this move');
  await invoke('resume_move', { taskId: task.id, sourceConfig, destConfig });
}
