'use client';

import { Button, Tooltip } from 'antd';
import { BugOutlined } from '@ant-design/icons';
import { useReportStore } from '@/app/stores/reportStore';

/** Status-bar trigger for the "Report a problem" dialog, next to Check for updates. */
export default function ReportProblemButton() {
  const open = useReportStore((s) => s.open);

  return (
    <Tooltip title="Report a problem">
      <Button
        type="text"
        size="small"
        icon={<BugOutlined />}
        onClick={open}
        aria-label="Report a problem"
      />
    </Tooltip>
  );
}
