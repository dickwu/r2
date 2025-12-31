'use client';

interface DomainInfoProps {
  currentConfig: {
    account_id: string;
    public_domain?: string | null;
    access_key_id?: string | null;
  } | null;
}

export default function DomainInfo({ currentConfig }: DomainInfoProps) {
  if (!currentConfig) {
    return null;
  }

  return (
    <span className="domain">
      {currentConfig.public_domain
        ? currentConfig.public_domain
        : currentConfig.access_key_id
          ? `${currentConfig.account_id}.r2.cloudflarestorage.com (signed)`
          : `${currentConfig.account_id}.r2.cloudflarestorage.com`}
    </span>
  );
}
