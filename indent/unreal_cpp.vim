" Reuse Neovim's C++ indentation for Unreal C++ buffers.
" 复用 Neovim 的 C++ 缩进，并修正 Unreal 宏/分号换行的额外缩进。
runtime! indent/cpp.vim

function! GetUnrealCppIndent() abort
  let l:lnum = prevnonblank(v:lnum - 1)
  if l:lnum > 0
    let l:line = getline(l:lnum)

    if get(g:, 'ucore_indent_unreal_macro', 1)
          \ && l:line =~# '^\s*[A-Z][A-Z0-9_]\+\s*(.*)\s*$'
          \ && l:line !~# '(\s*$'
      return indent(l:lnum)
    endif

    if get(g:, 'ucore_indent_semicolon', 1)
          \ && l:line =~# ';\s*$'
          \ && l:line !~# '^\s*}'
      return indent(l:lnum)
    endif
  endif

  return cindent(v:lnum)
endfunction

let b:did_indent = 1
setlocal autoindent
setlocal nosmartindent
setlocal nocindent
setlocal indentexpr=GetUnrealCppIndent()
setlocal indentkeys=0{,0},0),0],!^F,o,O,e
