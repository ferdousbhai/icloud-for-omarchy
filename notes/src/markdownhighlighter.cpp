#include "markdownhighlighter.h"

#include <QRegularExpression>
#include <QTextBlock>
#include <QTextCharFormat>
#include <QTextDocument>

MarkdownHighlighter::MarkdownHighlighter(QTextDocument *document, const Colors &colors)
    : QSyntaxHighlighter(document), m_colors(colors)
{
}

void MarkdownHighlighter::setColors(const Colors &colors)
{
    m_colors = colors;
    rehighlight();
}

void MarkdownHighlighter::setActivePosition(int position)
{
    const int block = position < 0 ? -1 : document()->findBlock(position).blockNumber();
    if (block == m_activeBlock)
        return;
    const int previous = m_activeBlock;
    m_activeBlock = block;
    for (int number : { previous, block })
        if (const QTextBlock b = document()->findBlockByNumber(number); b.isValid())
            rehighlightBlock(b);
}

void MarkdownHighlighter::highlightBlock(const QString &text)
{
    static const QRegularExpression heading(QStringLiteral(R"(^(#{1,6})\s+)"));
    static const QRegularExpression checkbox(QStringLiteral(R"(^\s*(?:[-*+]|\d+[.)])\s+(\[( |x|X)\])\s)"));
    static const QRegularExpression listMarker(QStringLiteral(R"(^\s*([-*+]|\d+[.)])\s)"));
    static const QRegularExpression quote(QStringLiteral(R"(^\s*>)"));
    static const QRegularExpression rule(QStringLiteral(R"(^\s*([-*_])\s*\1\s*\1[\s\-*_]*$)"));
    static const QRegularExpression bold(QStringLiteral(R"((\*\*|__)(?=\S)(.+?)(?<=\S)\1)"));
    static const QRegularExpression italic(QStringLiteral(R"((?<![*\w])(\*|_)(?=\S)([^*_]+?)(?<=\S)\1(?![*\w]))"));
    static const QRegularExpression strike(QStringLiteral(R"(~~(?=\S)(.+?)(?<=\S)~~)"));
    static const QRegularExpression code(QStringLiteral(R"(`([^`]+)`)"));
    static const QRegularExpression link(QStringLiteral(R"((!?\[)([^\]]*)(\]\([^)]*\)))"));

    QTextCharFormat mutedFormat;
    mutedFormat.setForeground(m_colors.muted);
    QTextCharFormat accentFormat;
    accentFormat.setForeground(m_colors.accent);
    // Off the cursor line, marks shrink to a sliver and turn transparent.
    QTextCharFormat hiddenFormat;
    hiddenFormat.setForeground(Qt::transparent);
    hiddenFormat.setProperty(QTextFormat::FontPixelSize, 1);
    const bool active = currentBlock().blockNumber() == m_activeBlock;
    auto mark = [&](qsizetype start, qsizetype length) {
        setFormat(int(start), int(length), active ? mutedFormat : hiddenFormat);
    };

    // Line shapes first, so inline emphasis inside them still shows.
    QRegularExpressionMatch m = heading.match(text);
    if (m.hasMatch()) {
        QTextCharFormat headingFormat;
        headingFormat.setFontWeight(QFont::Bold);
        const int level = int(m.captured(1).size());
        const double scale = level == 1 ? 1.6 : level == 2 ? 1.25 : 1.08;
        // The editor sets its font in pixels; scale whichever unit it uses.
        const QFont base = document()->defaultFont();
        if (base.pixelSize() > 0)
            headingFormat.setProperty(QTextFormat::FontPixelSize, int(base.pixelSize() * scale));
        else
            headingFormat.setFontPointSize(base.pointSizeF() * scale);
        setFormat(0, int(text.size()), headingFormat);
        mark(0, m.capturedLength(0));
    } else if ((m = rule.match(text)).hasMatch()) {
        setFormat(0, int(text.size()), mutedFormat);
    } else if ((m = quote.match(text)).hasMatch()) {
        QTextCharFormat quoteFormat;
        quoteFormat.setForeground(m_colors.dim);
        quoteFormat.setFontItalic(true);
        setFormat(0, int(text.size()), quoteFormat);
        setFormat(0, int(m.capturedLength(0)), accentFormat);
    } else if ((m = checkbox.match(text)).hasMatch()) {
        if (m.captured(2) != u" ") {
            QTextCharFormat doneFormat;
            doneFormat.setForeground(m_colors.muted);
            doneFormat.setFontStrikeOut(true);
            setFormat(int(m.capturedEnd(1)), int(text.size() - m.capturedEnd(1)), doneFormat);
        }
        setFormat(0, int(m.capturedEnd(1)), accentFormat);
    } else if ((m = listMarker.match(text)).hasMatch()) {
        setFormat(int(m.capturedStart(1)), int(m.capturedLength(1)), accentFormat);
    }

    auto eachMatch = [&](const QRegularExpression &re, auto apply) {
        for (const QRegularExpressionMatch &match : re.globalMatch(text))
            apply(match);
    };
    eachMatch(bold, [&](const QRegularExpressionMatch &match) {
        QTextCharFormat f = format(int(match.capturedStart(2)));
        f.setFontWeight(QFont::Bold);
        setFormat(int(match.capturedStart(2)), int(match.capturedLength(2)), f);
        mark(match.capturedStart(0), match.capturedLength(1));
        mark(match.capturedEnd(2), match.capturedLength(1));
    });
    eachMatch(italic, [&](const QRegularExpressionMatch &match) {
        QTextCharFormat f = format(int(match.capturedStart(2)));
        f.setFontItalic(true);
        setFormat(int(match.capturedStart(2)), int(match.capturedLength(2)), f);
        mark(match.capturedStart(0), 1);
        mark(match.capturedEnd(2), 1);
    });
    eachMatch(strike, [&](const QRegularExpressionMatch &match) {
        QTextCharFormat f = format(int(match.capturedStart(1)));
        f.setFontStrikeOut(true);
        setFormat(int(match.capturedStart(1)), int(match.capturedLength(1)), f);
        mark(match.capturedStart(0), 2);
        mark(match.capturedEnd(1), 2);
    });
    eachMatch(code, [&](const QRegularExpressionMatch &match) {
        QTextCharFormat f;
        f.setFontFamilies({ QStringLiteral("monospace") });
        f.setBackground(m_colors.code);
        setFormat(int(match.capturedStart(0)), int(match.capturedLength(0)), f);
        if (!active) {
            mark(match.capturedStart(0), 1);
            mark(match.capturedEnd(0) - 1, 1);
        }
    });
    eachMatch(link, [&](const QRegularExpressionMatch &match) {
        mark(match.capturedStart(1), match.capturedLength(1));
        QTextCharFormat f = format(int(match.capturedStart(2)));
        f.setForeground(m_colors.accent);
        f.setFontUnderline(true);
        setFormat(int(match.capturedStart(2)), int(match.capturedLength(2)), f);
        mark(match.capturedStart(3), match.capturedLength(3));
    });
}
