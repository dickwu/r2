'use client';

import { Alert, Progress, Skeleton } from 'antd';
import { CloudDownloadOutlined } from '@ant-design/icons';
import { useShallow } from 'zustand/react/shallow';
import Modal from '@/app/components/ui/Modal';
import UpdateNotes, { openExternal } from '@/app/components/updates/UpdateNotes';
import { useActiveWork } from '@/app/hooks/useActiveWork';
import { describeActiveWork, hasActiveWork } from '@/app/lib/updates/activeWork';
import { githubReleaseUrl } from '@/app/lib/updates/releaseNotes';
import { progressAmount, progressPercent } from '@/app/lib/updates/updateStatus';
import { isUpdateBusy, updateNotes, useUpdateStore } from '@/app/stores/updateStore';

function UpdateDialog() {
  const s = useUpdateStore(
    useShallow((state) => ({
      phase: state.phase,
      update: state.update,
      currentVersion: state.currentVersion,
      progress: state.progress,
      failure: state.failure,
      feed: state.feed,
      feedStatus: state.feedStatus,
      install: state.install,
      restartNow: state.restartNow,
      retry: state.retry,
      skipThisVersion: state.skipThisVersion,
      closeDialog: state.closeDialog,
    }))
  );
  const work = useActiveWork();
  if (!s.update) return null;

  const { phase, failure } = s;
  const version = s.update.version;
  const busy = isUpdateBusy(phase);
  const installed =
    phase === 'readyToRestart' ||
    phase === 'restarting' ||
    (phase === 'error' && failure?.step === 'restart');
  const workAtRisk =
    hasActiveWork(work) &&
    (phase === 'available' || phase === 'readyToRestart' || phase === 'error');
  const notes = updateNotes(s.feed, s.currentVersion, s.update);
  const releaseUrl = githubReleaseUrl(version);
  const percent = progressPercent(s.progress);

  const title = installed ? 'Restart to finish updating' : `Update to v${version}`;
  const subtitle = installed
    ? `v${version} is installed. It starts the next time R2 Client opens.`
    : s.currentVersion
      ? `You have v${s.currentVersion}.`
      : undefined;

  const downloadFromGitHub = (
    <button type="button" className="btn btn-ghost" onClick={() => openExternal(releaseUrl)}>
      Download from GitHub
    </button>
  );

  let footer: React.ReactNode;
  if (phase === 'error' && failure) {
    footer = (
      <>
        {failure.step !== 'restart' && <span className="left">{downloadFromGitHub}</span>}
        <button type="button" className="btn" onClick={s.closeDialog}>
          Later
        </button>
        {failure.retryable || failure.step === 'restart' ? (
          <button type="button" className="btn btn-primary" onClick={() => void s.retry()}>
            Try again
          </button>
        ) : (
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => openExternal(releaseUrl)}
          >
            Download from GitHub
          </button>
        )}
      </>
    );
  } else if (installed) {
    footer = (
      <>
        <button type="button" className="btn" onClick={s.closeDialog} disabled={busy}>
          Later
        </button>
        <button
          type="button"
          className="btn btn-primary"
          onClick={() => void s.restartNow()}
          disabled={busy}
        >
          {phase === 'restarting' ? 'Restarting…' : 'Restart now'}
        </button>
      </>
    );
  } else if (busy) {
    footer = (
      <button type="button" className="btn" onClick={s.closeDialog}>
        Hide
      </button>
    );
  } else {
    footer = (
      <>
        <span className="left">
          <button type="button" className="btn btn-ghost" onClick={s.skipThisVersion}>
            Skip this version
          </button>
        </span>
        <button type="button" className="btn" onClick={s.closeDialog}>
          Later
        </button>
        {workAtRisk ? (
          <>
            <button type="button" className="btn" onClick={() => void s.install({ restart: true })}>
              Update and restart
            </button>
            <button
              type="button"
              className="btn btn-primary"
              onClick={() => void s.install({ restart: false })}
            >
              Install now, restart later
            </button>
          </>
        ) : (
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => void s.install({ restart: true })}
          >
            Update and restart
          </button>
        )}
      </>
    );
  }

  return (
    <Modal
      open
      onClose={s.closeDialog}
      title={title}
      subtitle={subtitle}
      icon={<CloudDownloadOutlined style={{ fontSize: 18 }} />}
      width={560}
      footer={footer}
    >
      <div className="update-dialog">
        {(phase === 'downloading' || phase === 'installing') && (
          <div className="update-progress" aria-live="polite">
            <div className="update-progress-label">
              <span>
                {phase === 'installing'
                  ? 'Installing…'
                  : `Downloading ${progressAmount(s.progress)}`}
              </span>
              {phase === 'downloading' && percent !== null && <span>{percent}%</span>}
            </div>
            <Progress
              percent={phase === 'installing' ? 100 : (percent ?? 0)}
              showInfo={false}
              size="small"
              status="active"
            />
          </div>
        )}

        {phase === 'error' && failure && (
          <Alert
            type="error"
            showIcon
            title={failure.message}
            description={
              <details className="update-error-detail">
                <summary>Details</summary>
                <code>{failure.detail}</code>
              </details>
            }
          />
        )}

        {workAtRisk && (
          <Alert
            type="warning"
            showIcon
            title={`Restarting now would ${describeActiveWork(work)}.`}
            description={
              installed
                ? 'Restart when they are done.'
                : 'Install now, restart later installs the update and keeps them going. Restart when they are done.'
            }
          />
        )}

        {!installed && (
          <div className="update-notes-box">
            {notes.source === 'none' && (s.feedStatus === 'loading' || s.feedStatus === 'idle') ? (
              <Skeleton active title={false} paragraph={{ rows: 4 }} />
            ) : notes.source === 'none' ? (
              <p className="update-notes-missing">
                There are no release notes for this version here yet.
              </p>
            ) : (
              <UpdateNotes markdown={notes.markdown} />
            )}
          </div>
        )}

        <button type="button" className="update-link" onClick={() => openExternal(releaseUrl)}>
          Release notes on GitHub
        </button>
      </div>
    </Modal>
  );
}

/** The update dialog, mounted only while open. */
export default function UpdateModal() {
  const open = useUpdateStore((s) => s.dialogOpen && s.update !== null);
  return open ? <UpdateDialog /> : null;
}
