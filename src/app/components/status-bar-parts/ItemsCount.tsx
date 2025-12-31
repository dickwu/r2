'use client';

interface ItemsCountProps {
  hasConfig: boolean;
  searchQuery: string;
  searchTotalCount: number;
  totalItemsCount: number;
}

export default function ItemsCount({
  hasConfig,
  searchQuery,
  searchTotalCount,
  totalItemsCount,
}: ItemsCountProps) {
  if (!hasConfig) {
    return null;
  }

  return (
    <span>
      {searchQuery
        ? `${searchTotalCount.toLocaleString()} result${searchTotalCount !== 1 ? 's' : ''}`
        : `${totalItemsCount} items`}
    </span>
  );
}
