$url = "https://www.python.org/ftp/python/3.11.9/python-3.11.9-embed-amd64.zip"
$dest = "$PSScriptRoot\..\src-tauri\resources\python-embed.zip"
Write-Host "Downloading Python 3.11.9 embeddable..."
Invoke-WebRequest -Uri $url -OutFile $dest
Write-Host "Done: $dest"
