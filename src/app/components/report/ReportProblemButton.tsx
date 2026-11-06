'use client';

import { useTranslation } from 'react-i18next';
import { Button, Tooltip } from 'antd';
import { BugOutlined } from '@ant-design/icons';
import { useReportStore } from '@/app/stores/reportStore';

/** Status-bar trigger for the "Report a problem" dialog, next to Check for updates. */
export default function ReportProblemButton() {
  const open = useReportStore((s) => s.open);
  const { t } = useTranslation();

  return (
    <Tooltip title={t('report.trigger')}>
      <Button
        type="text"
        size="small"
        icon={<BugOutlined />}
        onClick={() => open()}
        aria-label={t('report.trigger')}
      />
    </Tooltip>
  );
}
