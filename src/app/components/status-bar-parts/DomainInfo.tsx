'use client';

import { buildBucketBaseUrl, StorageConfig } from '../../lib/r2cache';

interface DomainInfoProps {
  storageConfig: StorageConfig | null;
}

export default function DomainInfo({ storageConfig }: DomainInfoProps) {
  if (!storageConfig) {
    return null;
  }

  const baseUrl = buildBucketBaseUrl(storageConfig);
  const isSigned = !storageConfig.publicDomain;

  return (
    <span className="domain">
      {baseUrl ? baseUrl.replace(/^https?:\/\//, '') : 'Unknown'}
      {isSigned ? ' (signed)' : ''}
    </span>
  );
}
