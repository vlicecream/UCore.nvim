" Reuse Neovim's C++ indentation for Unreal C++ buffers.
" 复用 Neovim 的 C++ 缩进，让 ft=unreal_cpp 拥有正常代码块回车缩进。
runtime! indent/cpp.vim

if exists("*GetUnrealCppIndent")
  finish
endif

function! GetUnrealCppIndent() abort
  let l:lnum = prevnonblank(v:lnum - 1)
  if l:lnum > 0
    let l:line = getline(l:lnum)
    if l:line =~# '^\s*U[A-Z0-9_]\+\s*(.*)\s*$' && l:line !~# '(\s*$'
      return indent(l:lnum)
    endif
  endif

  return cindent(v:lnum)
endfunction

let b:did_indent = 1
setlocal autoindent
setlocal cindent
setlocal indentexpr=GetUnrealCppIndent()
