#ifndef MARKDOWNHIGHLIGHTER_H
#define MARKDOWNHIGHLIGHTER_H

#include <QColor>
#include <QSyntaxHighlighter>

// Styles Markdown in the editor the way Notes renders it: headings large,
// emphasis shown, checklist marks and links in the accent colour, done
// items struck through. As in Typora, heading, emphasis, code and link
// marks show only on the line being edited and shrink out of sight
// elsewhere. It only formats; the text stays byte-for-byte the Markdown
// icloud-md wrote, so nothing here can create a sync diff.
class MarkdownHighlighter : public QSyntaxHighlighter
{
    Q_OBJECT
public:
    struct Colors {
        QColor accent;
        QColor muted;
        QColor dim;
        QColor code;
    };

    explicit MarkdownHighlighter(QTextDocument *document, const Colors &colors);
    void setColors(const Colors &colors);
    // The cursor's document position, or -1 when the editor has no focus.
    void setActivePosition(int position);

protected:
    void highlightBlock(const QString &text) override;

private:
    Colors m_colors;
    int m_activeBlock = -1;
};

#endif
