'use client';

import { useEffect, useState, useRef, useMemo } from 'react';
import { Document, Page, pdfjs } from 'react-pdf';
import { Button, Space, Spin } from 'antd';
import { LeftOutlined, RightOutlined, ZoomInOutlined, ZoomOutOutlined } from '@ant-design/icons';
import { invoke } from '@tauri-apps/api/core';
import DownloadProgress from './DownloadProgress';
import { useThemeStore } from '../../stores/themeStore';
import 'react-pdf/dist/Page/TextLayer.css';
import 'react-pdf/dist/Page/AnnotationLayer.css';

interface PDFViewerProps {
  url: string;
  showControls?: boolean;
  showAllPages?: boolean;
  initialScale?: number;
  maxHeight?: string;
  disableInternalScroll?: boolean;
  onLoadSuccess?: (numPages: number) => void;
  onLoadError?: (error: Error) => void;
}

export default function PDFViewer({
  url,
  showControls = false,
  showAllPages = true,
  initialScale,
  maxHeight = '700px',
  disableInternalScroll = false,
  onLoadSuccess,
  onLoadError,
}: PDFViewerProps) {
  const [numPages, setNumPages] = useState<number>(0);
  const [currentPage, setCurrentPage] = useState<number>(1);
  const [isMobile, setIsMobile] = useState<boolean>(false);
  const [pdfData, setPdfData] = useState<Uint8Array | null>(null);
  const [fetchLoading, setFetchLoading] = useState<boolean>(false);
  const [fetchError, setFetchError] = useState<string | null>(null);
  const [downloadProgress, setDownloadProgress] = useState<{
    loaded: number;
    total: number | null;
  }>({ loaded: 0, total: null });
  const containerRef = useRef<HTMLDivElement>(null);
  const pageRefs = useRef<Map<number, HTMLDivElement>>(new Map());

  // Calculate responsive scale based on device
  const getResponsiveScale = () => {
    if (typeof window === 'undefined') return 1.2;
    const width = window.innerWidth;
    if (width < 640) return 0.5; // Mobile: very small
    if (width < 1024) return 0.8; // Tablet: medium
    return initialScale || 1.2; // Desktop: normal
  };

  const [scale, setScale] = useState<number>(getResponsiveScale());

  useEffect(() => {
    pdfjs.GlobalWorkerOptions.workerSrc = `https://unpkg.com/pdfjs-dist@${pdfjs.version}/build/pdf.worker.min.mjs`;

    // Detect mobile device and handle responsive resize
    const handleResize = () => {
      const width = window.innerWidth;
      const mobile = width < 768;
      setIsMobile(mobile);
      setScale(getResponsiveScale());
    };

    // Initial check
    handleResize();

    // Listen to window resize
    window.addEventListener('resize', handleResize);
    return () => window.removeEventListener('resize', handleResize);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Track visible pages with IntersectionObserver
  useEffect(() => {
    if (!showAllPages || !containerRef.current) return;

    const observer = new IntersectionObserver(
      (entries) => {
        // Find the most visible page
        let maxRatio = 0;
        let mostVisiblePage = currentPage;

        entries.forEach((entry) => {
          if (entry.isIntersecting && entry.intersectionRatio > maxRatio) {
            maxRatio = entry.intersectionRatio;
            const pageNum = parseInt(entry.target.getAttribute('data-page-number') || '1');
            mostVisiblePage = pageNum;
          }
        });

        if (maxRatio > 0.3) {
          setCurrentPage(mostVisiblePage);
        }
      },
      {
        root: containerRef.current,
        threshold: [0, 0.3, 0.5, 0.7, 1.0],
      }
    );

    // Observe all page elements
    pageRefs.current.forEach((element) => {
      observer.observe(element);
    });

    return () => observer.disconnect();
  }, [numPages, showAllPages, currentPage]);

  // Fetch PDF via Tauri backend to bypass HTTP scope restrictions
  useEffect(() => {
    if (!url) {
      setPdfData(null);
      return;
    }

    let cancelled = false;
    setFetchLoading(true);
    setFetchError(null);
    setPdfData(null);
    setCurrentPage(1);
    setDownloadProgress({ loaded: 0, total: null });

    const fetchPdf = async () => {
      try {
        // Use Tauri command to fetch PDF bytes (returns base64-encoded string)
        const base64Data = await invoke<string>('fetch_url_bytes', { url });

        if (cancelled) return;

        // Decode base64 to Uint8Array
        const binaryString = atob(base64Data);
        const bytes = new Uint8Array(binaryString.length);
        for (let i = 0; i < binaryString.length; i++) {
          bytes[i] = binaryString.charCodeAt(i);
        }

        setDownloadProgress({ loaded: bytes.length, total: bytes.length });
        setPdfData(bytes);
      } catch (err) {
        if (!cancelled) {
          console.error('Failed to fetch PDF:', err);
          const errorMsg = err instanceof Error ? err.message : 'Failed to load PDF';
          setFetchError(errorMsg);
          onLoadError?.(err instanceof Error ? err : new Error(errorMsg));
        }
      } finally {
        if (!cancelled) {
          setFetchLoading(false);
        }
      }
    };

    fetchPdf();

    return () => {
      cancelled = true;
    };
  }, [url, onLoadError]);

  function handleDocumentLoadSuccess({ numPages }: { numPages: number }): void {
    setNumPages(numPages);
    // Don't reset currentPage here - it's handled by URL change effect
    // This prevents page jumping when navigating
    onLoadSuccess?.(numPages);
  }

  function handleDocumentLoadError(error: Error): void {
    console.error('PDF load error:', error);
    onLoadError?.(error);
  }

  const scrollToPage = (pageNumber: number) => {
    const pageElement = pageRefs.current.get(pageNumber);
    if (pageElement && containerRef.current) {
      pageElement.scrollIntoView({ behavior: 'smooth', block: 'start' });
    }
  };

  const goToPreviousPage = () => {
    const newPage = Math.max(currentPage - 1, 1);
    if (showAllPages) {
      scrollToPage(newPage);
    } else {
      setCurrentPage(newPage);
      if (containerRef.current) {
        containerRef.current.scrollTop = 0;
      }
    }
  };

  const goToNextPage = () => {
    const newPage = Math.min(currentPage + 1, numPages);
    if (showAllPages) {
      scrollToPage(newPage);
    } else {
      setCurrentPage(newPage);
      if (containerRef.current) {
        containerRef.current.scrollTop = 0;
      }
    }
  };

  const handleZoomIn = () => {
    setScale((prev) => Math.min(prev + 0.1, 3.0));
  };

  const handleZoomOut = () => {
    setScale((prev) => Math.max(prev - 0.1, 0.5));
  };

  // Memoize file object to prevent unnecessary reloads
  const file = useMemo(() => (pdfData ? { data: pdfData } : null), [pdfData]);

  const appTheme = useThemeStore((s) => s.theme);
  const isDark = appTheme === 'dark';

  return (
    <div className="pdf-viewer-wrapper">
      {/* PDF Controls - Responsive */}
      {showControls && (
        <div
          className={`mb-3 flex flex-col gap-2 rounded-lg border p-2 sm:flex-row sm:items-center sm:justify-between sm:p-3 ${
            isDark ? 'border-gray-700 bg-gray-800' : 'border-gray-200 bg-gray-50'
          }`}
        >
          <Space size="small" className="flex-wrap justify-center sm:justify-start">
            <Button
              icon={<LeftOutlined />}
              onClick={goToPreviousPage}
              disabled={currentPage <= 1}
              size="small"
            >
              {isMobile ? '' : 'Previous'}
            </Button>
            <span className="text-xs sm:text-sm">
              {currentPage}/{numPages}
            </span>
            <Button
              icon={<RightOutlined />}
              onClick={goToNextPage}
              disabled={currentPage >= numPages}
              size="small"
            >
              {isMobile ? '' : 'Next'}
            </Button>
          </Space>
          <Space size="small" className="flex-wrap justify-center sm:justify-end">
            <Button
              icon={<ZoomOutOutlined />}
              onClick={handleZoomOut}
              disabled={scale <= 0.5}
              size="small"
            >
              {isMobile ? '' : 'Zoom Out'}
            </Button>
            <span className="text-xs sm:text-sm">{Math.round(scale * 100)}%</span>
            <Button
              icon={<ZoomInOutlined />}
              onClick={handleZoomIn}
              disabled={scale >= 3.0}
              size="small"
            >
              {isMobile ? '' : 'Zoom In'}
            </Button>
          </Space>
        </div>
      )}

      {/* PDF Document - Responsive Container */}
      <div
        ref={containerRef}
        className={`flex justify-center rounded border ${disableInternalScroll ? 'overflow-visible' : 'overflow-auto'}`}
        style={{
          maxHeight: disableInternalScroll ? 'none' : maxHeight,
          backgroundColor: '#525659',
        }}
      >
        <div className="flex w-full flex-col items-center px-2 sm:px-0">
          {fetchLoading ? (
            <DownloadProgress loaded={downloadProgress.loaded} total={downloadProgress.total} />
          ) : fetchError ? (
            <div className="flex h-96 w-full items-center justify-center self-center">
              <div className="py-4 text-red-400">Error: {fetchError}</div>
            </div>
          ) : !file ? (
            <div className="flex h-96 w-full items-center justify-center self-center">
              <div className="py-4 text-white">No PDF to display</div>
            </div>
          ) : (
            <Document
              file={file}
              onLoadSuccess={handleDocumentLoadSuccess}
              onLoadError={handleDocumentLoadError}
              loading={
                <div className="flex h-96 w-full items-center justify-center self-center">
                  <Spin spinning={true}>
                    <div className="py-4 text-white">Rendering PDF...</div>
                  </Spin>
                </div>
              }
            >
              {showAllPages && !isMobile
                ? Array.from(new Array(numPages), (_, index) => {
                    const pageNum = index + 1;
                    return (
                      <div
                        key={`page_${pageNum}`}
                        ref={(el) => {
                          if (el) {
                            pageRefs.current.set(pageNum, el);
                          }
                        }}
                        data-page-number={pageNum}
                      >
                        <Page
                          pageNumber={pageNum}
                          scale={scale}
                          renderTextLayer={true}
                          renderAnnotationLayer={true}
                          className="my-2 sm:my-4"
                          width={isMobile ? window.innerWidth - 40 : undefined}
                        />
                      </div>
                    );
                  })
                : numPages > 0 && (
                    <Page
                      pageNumber={currentPage}
                      scale={scale}
                      renderTextLayer={true}
                      renderAnnotationLayer={true}
                      className="my-2 sm:my-4"
                      width={isMobile ? window.innerWidth - 40 : undefined}
                    />
                  )}
            </Document>
          )}
        </div>
      </div>
    </div>
  );
}
