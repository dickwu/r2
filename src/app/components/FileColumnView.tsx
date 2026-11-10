/**
 * FileColumnView — Finder-style column browser.
 *
 * Each folder from the bucket root down to `currentPath` is one column; the
 * current folder's contents are the last column. Clicking a folder opens it
 * in the next column (by setting `currentPath`, so breadcrumbs, upload target
 * and status bar follow along); clicking a file selects it and shows a preview
 * column. Double-click or Enter opens the file preview, ⌘/Ctrl-click toggles a
 * row in the batch selection, and the arrow keys walk the columns.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Spin } from 'antd';
import { RightOutlined } from '@ant-design/icons';
import { Virtuoso } from 'react-virtuoso';
import type { FileItem } from '@/app/hooks/useR2Files';
import type { StorageConfig } from '@/app/lib/r2cache';
import { useThemeStore } from '@/app/stores/themeStore';
import { useFolderColumns, sortedOnce, type FolderColumn } from '@/app/hooks/useFolderColumns';
import {
  columnKeyAction,
  columnPrefixes,
  highlightedKey,
  pickItem,
  visibleFileSelection,
  type ColumnFileSelection,
  type ColumnKey,
  type ColumnKeyAction,
} from '@/app/utils/columnView';
import FileContextMenu from '@/app/components/FileContextMenu';
import { toneAndIcon } from '@/app/components/FileListRow';
import ColumnPreview from '@/app/components/ColumnPreview';

interface FileColumnViewProps {
  config: StorageConfig | null;
  currentPath: string;
  /** The page's listing of `currentPath` (unsorted); shown as the last column. */
  currentItems: FileItem[];
  currentLoading: boolean;
  selectedKeys: Set<string>;
  onNavigate: (path: string) => void;
  onOpen: (file: FileItem, siblings: FileItem[]) => void;
  onToggleSelection: (key: string) => void;
  onDelete: (item: FileItem) => void;
  onRename: (item: FileItem) => void;
  onDownload?: (item: FileItem) => void;
  onFolderDelete?: (item: FileItem) => void;
  onFolderDownload?: (item: FileItem) => void;
  onFolderRename?: (item: FileItem) => void;
}

const COLUMN_KEYS = new Set<string>(['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Enter']);

function prefersReducedMotion(): boolean {
  return (
    typeof window !== 'undefined' && window.matchMedia('(prefers-reduced-motion: reduce)').matches
  );
}

export default function FileColumnView({
  config,
  currentPath,
  currentItems,
  currentLoading,
  selectedKeys,
  onNavigate,
  onOpen,
  onToggleSelection,
  onDelete,
  onRename,
  onDownload,
  onFolderDelete,
  onFolderDownload,
  onFolderRename,
}: FileColumnViewProps) {
  const { t } = useTranslation();
  const density = useThemeStore((s) => s.density);
  const scrollerRef = useRef<HTMLDivElement | null>(null);
  const [fileSelection, setFileSelection] = useState<ColumnFileSelection | null>(null);

  const prefixes = useMemo(() => columnPrefixes(currentPath), [currentPath]);
  const ancestorPrefixes = useMemo(() => prefixes.slice(0, -1), [prefixes]);
  const ancestors = useFolderColumns(config, ancestorPrefixes);
  const columns: FolderColumn[] = [
    ...ancestors,
    {
      prefix: currentPath,
      items: sortedOnce(currentItems),
      isLoading: currentLoading,
      error: null,
    },
  ];
  const lastItems = columns[columns.length - 1].items;

  // A selected file stays only while its folder is the last column and it still exists.
  const visible = visibleFileSelection(fileSelection, currentPath);
  const selection =
    visible && lastItems.some((item) => item.key === visible.file.key) ? visible : null;

  const apply = useCallback(
    (action: ColumnKeyAction | null, siblings: FileItem[] = lastItems) => {
      if (!action) return;
      switch (action.type) {
        case 'navigate':
          setFileSelection(null);
          if (action.path !== currentPath) onNavigate(action.path);
          break;
        case 'selectFile':
          setFileSelection(action.selection);
          if (action.selection.parent !== currentPath) onNavigate(action.selection.parent);
          break;
        case 'clearFile':
          setFileSelection(null);
          break;
        case 'open':
          onOpen(action.file, siblings);
          break;
      }
    },
    [currentPath, lastItems, onNavigate, onOpen]
  );

  const handleKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (!COLUMN_KEYS.has(e.key) || e.metaKey || e.ctrlKey || e.altKey) return;
    e.preventDefault();
    apply(columnKeyAction(columns, currentPath, selection, e.key as ColumnKey));
  };

  // Keep the newest column (or the preview) in view as the trail grows.
  const trailKey = `${currentPath}|${selection?.file.key ?? ''}`;
  useEffect(() => {
    const el = scrollerRef.current;
    if (!el) return;
    el.scrollTo({
      left: el.scrollWidth,
      behavior: prefersReducedMotion() ? 'auto' : 'smooth',
    });
  }, [trailKey]);

  const lastIndex = columns.length - 1;
  // The deepest highlight is "active"; highlights further left show the trail.
  const activeColumn = selection ? lastIndex : lastIndex - 1;

  return (
    <div
      ref={scrollerRef}
      className={['cv-scroller', density !== 'default' ? density : ''].filter(Boolean).join(' ')}
      tabIndex={0}
      aria-label={t('toolbar.columnView')}
      onKeyDown={handleKeyDown}
    >
      {columns.map((column, index) => (
        <ColumnPane
          key={column.prefix || '/'}
          column={column}
          highlighted={highlightedKey(prefixes, index, selection)}
          active={index === activeColumn}
          isLast={index === lastIndex}
          selectedKeys={selectedKeys}
          onPick={(item, e) => {
            scrollerRef.current?.focus({ preventScroll: true });
            if ((e.metaKey || e.ctrlKey) && index === lastIndex) {
              onToggleSelection(item.key);
              return;
            }
            apply(pickItem(item, column.prefix));
          }}
          onOpen={(item) => {
            if (!item.isFolder) onOpen(item, column.items);
          }}
          onDelete={onDelete}
          onRename={onRename}
          onDownload={onDownload}
          onFolderDelete={onFolderDelete}
          onFolderDownload={onFolderDownload}
          onFolderRename={onFolderRename}
        />
      ))}
      {selection && (
        <ColumnPreview
          key={selection.file.key}
          item={selection.file}
          config={config}
          onOpen={() => onOpen(selection.file, lastItems)}
          onDownload={onDownload}
        />
      )}
    </div>
  );
}

interface ColumnPaneProps {
  column: FolderColumn;
  highlighted: string | null;
  active: boolean;
  isLast: boolean;
  selectedKeys: Set<string>;
  onPick: (item: FileItem, e: React.MouseEvent) => void;
  onOpen: (item: FileItem) => void;
  onDelete: (item: FileItem) => void;
  onRename: (item: FileItem) => void;
  onDownload?: (item: FileItem) => void;
  onFolderDelete?: (item: FileItem) => void;
  onFolderDownload?: (item: FileItem) => void;
  onFolderRename?: (item: FileItem) => void;
}

function ColumnPane({
  column,
  highlighted,
  active,
  isLast,
  selectedKeys,
  onPick,
  onOpen,
  ...menuHandlers
}: ColumnPaneProps) {
  const { t } = useTranslation();
  const { items, isLoading, error } = column;
  const highlightIndex = highlighted ? items.findIndex((item) => item.key === highlighted) : -1;

  let body: React.ReactNode;
  if (isLoading && items.length === 0) {
    body = (
      <div className="cv-note">
        <Spin size="small" />
      </div>
    );
  } else if (error && items.length === 0) {
    body = <div className="cv-note">{t('columns.loadFailed')}</div>;
  } else if (items.length === 0) {
    body = <div className="cv-note">{t('columns.empty')}</div>;
  } else {
    body = (
      <Virtuoso
        style={{ flex: 1 }}
        data={items}
        computeItemKey={(_index, item) => item.key}
        initialTopMostItemIndex={
          highlightIndex > 0 ? { index: highlightIndex, align: 'center' } : 0
        }
        itemContent={(_index, item) => (
          <FileContextMenu item={item} {...menuHandlers}>
            <ColumnRow
              item={item}
              highlighted={item.key === highlighted}
              active={active}
              checked={isLast && selectedKeys.has(item.key)}
              onPick={onPick}
              onOpen={onOpen}
            />
          </FileContextMenu>
        )}
      />
    );
  }

  return <div className="cv-column">{body}</div>;
}

interface ColumnRowProps extends Omit<React.HTMLAttributes<HTMLDivElement>, 'onClick'> {
  ref?: React.Ref<HTMLDivElement>;
  item: FileItem;
  highlighted: boolean;
  active: boolean;
  checked: boolean;
  onPick: (item: FileItem, e: React.MouseEvent) => void;
  onOpen: (item: FileItem) => void;
}

/** Spreads the props FileContextMenu's Dropdown injects (onContextMenu, ref) onto the row. */
function ColumnRow({
  item,
  highlighted,
  active,
  checked,
  onPick,
  onOpen,
  className: injectedClassName,
  ...rest
}: ColumnRowProps) {
  const { tone, Icon } = toneAndIcon(item);
  const className = [
    'cv-row',
    highlighted ? (active ? 'is-active' : 'is-trail') : '',
    checked ? 'is-checked' : '',
    injectedClassName,
  ]
    .filter(Boolean)
    .join(' ');
  return (
    <div
      {...rest}
      className={className}
      title={item.name}
      onClick={(e) => onPick(item, e)}
      onDoubleClick={() => onOpen(item)}
    >
      <span className={`fl-icon ${tone}`}>
        <Icon style={{ fontSize: 12 }} />
      </span>
      <span className="cv-row-name">{item.name}</span>
      {item.isFolder && <RightOutlined className="cv-row-chevron" />}
    </div>
  );
}
