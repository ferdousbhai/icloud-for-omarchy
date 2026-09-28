QT += core gui qml quick quickcontrols2 printsupport dbus

CONFIG += c++17 release
TARGET = icloud-notes
TEMPLATE = app

HEADERS += \
    src/backgroundsync.h \
    src/notesbackend.h \
    src/vaultlock.h \
    src/markdownhighlighter.h

SOURCES += \
    src/main.cpp \
    src/backgroundsync.cpp \
    src/notesbackend.cpp \
    src/markdownhighlighter.cpp

RESOURCES += qml/resources.qrc
