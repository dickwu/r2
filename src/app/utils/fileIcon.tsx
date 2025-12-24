import {
  FileOutlined,
  FileImageOutlined,
  FileTextOutlined,
  FilePdfOutlined,
  FileZipOutlined,
  FileExcelOutlined,
  FileWordOutlined,
  FilePptOutlined,
  FileMarkdownOutlined,
  CodeOutlined,
  VideoCameraOutlined,
  AudioOutlined,
} from '@ant-design/icons';

export function getFileIcon(fileName: string) {
  const ext = fileName.toLowerCase().split('.').pop() || '';
  
  // Image files
  if (['jpg', 'jpeg', 'png', 'gif', 'webp', 'svg', 'bmp', 'ico'].includes(ext)) {
    return <FileImageOutlined className="icon file-icon" />;
  }
  
  // Video files
  if (['mp4', 'avi', 'mkv', 'mov', 'wmv', 'flv', 'webm', 'm4v'].includes(ext)) {
    return <VideoCameraOutlined className="icon file-icon" />;
  }
  
  // Audio files
  if (['mp3', 'wav', 'ogg', 'flac', 'aac', 'm4a', 'wma'].includes(ext)) {
    return <AudioOutlined className="icon file-icon" />;
  }
  
  // Document files
  if (['pdf'].includes(ext)) {
    return <FilePdfOutlined className="icon file-icon" />;
  }
  
  if (['doc', 'docx'].includes(ext)) {
    return <FileWordOutlined className="icon file-icon" />;
  }
  
  if (['xls', 'xlsx', 'csv'].includes(ext)) {
    return <FileExcelOutlined className="icon file-icon" />;
  }
  
  if (['ppt', 'pptx'].includes(ext)) {
    return <FilePptOutlined className="icon file-icon" />;
  }
  
  // Code files
  if (['js', 'jsx', 'ts', 'tsx', 'py', 'java', 'c', 'cpp', 'h', 'cs', 'php', 'rb', 'go', 'rs', 'swift', 'kt'].includes(ext)) {
    return <CodeOutlined className="icon file-icon" />;
  }
  
  // Markup/config files
  if (['html', 'htm', 'xml', 'json', 'yaml', 'yml', 'toml', 'ini', 'conf'].includes(ext)) {
    return <CodeOutlined className="icon file-icon" />;
  }
  
  // Text files
  if (['txt', 'log'].includes(ext)) {
    return <FileTextOutlined className="icon file-icon" />;
  }
  
  // Markdown
  if (['md', 'markdown'].includes(ext)) {
    return <FileMarkdownOutlined className="icon file-icon" />;
  }
  
  // Archive files
  if (['zip', 'rar', '7z', 'tar', 'gz', 'bz2', 'xz'].includes(ext)) {
    return <FileZipOutlined className="icon file-icon" />;
  }
  
  // Default
  return <FileOutlined className="icon file-icon" />;
}
